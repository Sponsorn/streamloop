//! The control process as `streamloop.exe control [--root DIR]` wires it: start order, a config save,
//! the busy port, the 3.0 leftovers and the exit codes, on a real socket with fakes behind it.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use streamloop_common::api::{ControlCmd, SupervisorStatus};
use streamloop_common::paths::{Layout, Root};
use streamloop_common::time::local_date;
use streamloop_common::version::VERSION;
use streamloop_control::app::{self, opens_window, App, EnvVars, ExitSwitch, Wiring, QUIT_LINE};
use streamloop_control::clock::SystemClock;
use streamloop_control::http::FakeHttp;
use streamloop_control::landing::LandedRecord;
use streamloop_control::launcher::{SpawnHandle, Spawner, StartKind};
use streamloop_control::autostart_store::AutostartStore;
use streamloop_control::leftovers::remove_3_0_leftovers;
use streamloop_control::runner::{FakeRunner, RunOutput};
use streamloop_control::supervisor_client::fake::{Call, FakeSupervisor};
use streamloop_control::supervisor_client::SupervisorApi;
use support::captured_logs;
use tempfile::TempDir;
use tokio::sync::{mpsc, Semaphore};

const OLD_HOOK: &str = "https://discord.example/old";
const NEW_HOOK: &str = "https://discord.example/new";
/// A release newer than this build, with the running major version.
const NEW_VERSION: &str = "3.9.9";
const NEW_EXE: &str = "new exe";

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Records each spawn and never starts a process. With a gate, each `launch` helper hands off only once the
/// gate gets a permit.
#[derive(Default)]
struct NoSpawner(Mutex<Vec<Vec<String>>>, Mutex<Option<Arc<Semaphore>>>);

struct Exited(Option<Arc<Semaphore>>);

#[async_trait]
impl SpawnHandle for Exited {
    async fn exit_code(self: Box<Self>) -> Option<i32> {
        if let Some(gate) = &self.0 {
            gate.acquire().await.expect("the gate stays open").forget();
        }
        Some(0)
    }
}

impl Spawner for NoSpawner {
    fn spawn(&self, _exe: &Path, args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>> {
        self.0.lock().unwrap().push(args.to_vec());
        Ok(Box::new(Exited(self.1.lock().unwrap().clone())))
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A supervisor of this exe's version, so the launcher attaches and never replaces it.
fn playing() -> SupervisorStatus {
    SupervisorStatus { state: Some("playing".into()), version: Some(VERSION.into()), ..SupervisorStatus::default() }
}

fn config(port: u16, playlist: &str) -> Value {
    json!({
        "port": port,
        "playlists": [{ "id": playlist }],
        "discord": { "webhookUrl": OLD_HOOK },
        "autoUpdateCheck": false,
        "autoUpdateYtdlp": false,
        "autoOpenAdmin": false,
        "twitchLivenessEnabled": false
    })
}

struct Rig {
    _dir: TempDir,
    root: PathBuf,
    port: u16,
    sup: Arc<FakeSupervisor>,
    http: Arc<FakeHttp>,
    /// GitHub and the downloads.
    updater_http: Arc<FakeHttp>,
    /// The encoder probes, `streamloop.exe check` and a staged exe's `--version`; `node --version` and yt-dlp
    /// never run, since neither is installed.
    runner: Arc<FakeRunner>,
    spawner: Arc<NoSpawner>,
    /// How many times the control process asked for the app's window.
    window_opens: Arc<AtomicUsize>,
    vars: EnvVars,
    autostart: Arc<streamloop_control::autostart_store::FakeAutostart>,
    /// The root is the exe's own folder: the updater stages, as in an install.
    from_exe: bool,
    /// Ctrl+C or the console closing.
    stop: mpsc::UnboundedSender<()>,
    stop_rx: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
}

impl Rig {
    fn new(playlist: &str) -> Rig {
        captured_logs();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("app")).unwrap();
        let port = free_port();
        std::fs::write(root.join("app").join("config.json"), config(port, playlist).to_string()).unwrap();
        let sup = Arc::new(FakeSupervisor::new());
        sup.set_status(Some(playing()));
        let http = Arc::new(FakeHttp::new());
        http.respond("https://discord.example/", 204, "");
        let runner = Arc::new(FakeRunner::new());
        let (stop, stop_rx) = mpsc::unbounded_channel();
        Rig {
            _dir: dir,
            root,
            port,
            sup,
            http,
            updater_http: Arc::new(FakeHttp::new()),
            runner,
            spawner: Arc::new(NoSpawner::default()),
            window_opens: Arc::new(AtomicUsize::new(0)),
            vars: EnvVars::default(),
            autostart: Arc::new(streamloop_control::autostart_store::FakeAutostart::new()),
            from_exe: false,
            stop,
            stop_rx: Mutex::new(Some(stop_rx)),
        }
    }

    fn layout(&self) -> Layout {
        Layout::new(&self.root)
    }

    fn wiring(&self) -> Wiring {
        let runner = self.runner.clone();
        Wiring {
            root: Root { dir: self.root.clone(), from_exe: self.from_exe },
            vars: self.vars.clone(),
            client: self.sup.clone(),
            http: self.http.clone(),
            updater_http: self.updater_http.clone(),
            runner: runner.clone(),
            spawner: self.spawner.clone(),
            clock: Arc::new(SystemClock),
            startup_dir: self.root.join("startup"),
            autostart: self.autostart.clone(),
            obs_running: Arc::new(|| false),
            kill_tree: Arc::new(|_| {}),
            pid_on_port: Arc::new(|_| None),
            protect: Arc::new(|_| Ok("blob".to_string())),
            setup_check: streamloop_control::api::setup_check_with(runner),
            open_window: {
                let opens = self.window_opens.clone();
                Arc::new(move || {
                    opens.fetch_add(1, Ordering::SeqCst);
                })
            },
            stop_signal: self.stop_rx.lock().unwrap().take().expect("one wiring per rig"),
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let path = rel.split('/').fold(self.root.clone(), |p, part| p.join(part));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// `_update\landed` as a landing writes it.
    fn landed(&self, to: &str, on_air: bool, held: bool) {
        let record = json!({ "from": "3.0.9", "to": to, "onAir": on_air, "held": held });
        self.write("_update/landed", record.to_string().as_bytes());
    }

    /// `data\update-rolled-back` as a rollback by START.bat leaves it.
    fn rolled_back(&self, held: bool) {
        let record = json!({ "from": VERSION, "to": NEW_VERSION, "onAir": !held, "held": held });
        self.write("data/update-rolled-back", record.to_string().as_bytes());
    }

    /// Merges `patch` into the config before the start.
    fn patch_config(&self, patch: Value) {
        let path = self.layout().config;
        let mut c: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (k, v) in patch.as_object().unwrap() {
            c[k] = v.clone();
        }
        std::fs::write(path, c.to_string()).unwrap();
    }

    /// From now on each `launch` helper hands off only when the returned gate gets a permit.
    fn hold_launches(&self) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.spawner.1.lock().unwrap() = Some(gate.clone());
        gate
    }

    fn spawns(&self) -> usize {
        self.spawner.0.lock().unwrap().len()
    }

    async fn timeline(&self) -> Vec<String> {
        self.get("/api/events").await.as_array().unwrap().iter().map(|e| e["message"].as_str().unwrap().to_string()).collect()
    }

    /// The bodies of the Discord posts to the configured webhook.
    fn discord_posts(&self) -> Vec<String> {
        self.http.requests().iter().filter(|r| r.url == OLD_HOOK).map(|r| String::from_utf8_lossy(r.body.as_deref().unwrap_or_default()).into_owned()).collect()
    }

    /// An exe and an ffmpeg, and a test encode that works: a launch would spawn.
    fn launchable(&self) {
        self.write("streamloop.exe", b"old exe");
        self.write("ffmpeg/ffmpeg.exe", b"");
        self.runner.push(Ok(RunOutput::ok("")));
    }

    /// A local release list offering `NEW_VERSION`, its exe served from 127.0.0.1 through the updater's fake.
    fn offer_new_version(&mut self) {
        let base = "http://127.0.0.1:9/";
        let assets: Vec<Value> = [format!("streamloop-v{NEW_VERSION}.zip"), format!("streamloop-v{NEW_VERSION}.exe"), format!("streamloop-v{NEW_VERSION}.exe.sha256")]
            .into_iter()
            .map(|name| json!({ "name": name, "browser_download_url": format!("{base}{name}") }))
            .collect();
        let list = json!([{ "tag_name": format!("v{NEW_VERSION}"), "draft": false, "prerelease": true, "html_url": "", "assets": assets }]);
        let file = self.root.parent().unwrap().join("releases.json");
        std::fs::write(&file, list.to_string()).unwrap();
        self.vars.test_releases = Some(file);
        self.updater_http.respond(&format!("{base}streamloop-v{NEW_VERSION}.exe"), 200, NEW_EXE);
        let checksum = format!("{}  streamloop-v{NEW_VERSION}.exe", sha256_hex(NEW_EXE.as_bytes()));
        self.updater_http.respond(&format!("{base}streamloop-v{NEW_VERSION}.exe.sha256"), 200, &checksum);
        self.from_exe = true;
    }

    async fn get(&self, path: &str) -> Value {
        json_of(reqwest::get(self.url(path)).await.unwrap()).await
    }

    async fn start(&self) -> App {
        match App::start(self.wiring()).await {
            Ok(app) => app,
            Err(code) => panic!("the control process did not start: exit {code}"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        let http = reqwest::Client::new();
        let token = json_of(http.get(self.url("/api/token")).send().await.unwrap()).await;
        let token = token["token"].as_str().unwrap().to_string();
        http.post(self.url(path)).header("x-api-token", token).header("content-type", "application/json").body(body.to_string()).send().await.unwrap()
    }

    fn status_polls(&self) -> usize {
        self.sup.calls().iter().filter(|c| **c == Call::Status).count()
    }

    fn supervisor_shutdowns(&self) -> usize {
        self.sup.calls().iter().filter(|c| **c == Call::Shutdown).count()
    }

    /// Starts with a supervisor that stops answering at `/shutdown` and a port 4417 that the returned hold keeps
    /// taken, so that a quit's stop lasts as long as the hold.
    async fn start_with_port_hold(&self) -> (App, PortHold) {
        self.sup.stop_answering_on_shutdown();
        let hold = PortHold::default();
        let mut wiring = self.wiring();
        let until = hold.0.clone();
        wiring.pid_on_port = Arc::new(move |_| until.lock().unwrap().filter(|t| Instant::now() < *t).map(|_| 4242));
        let app = App::start(wiring).await.unwrap_or_else(|code| panic!("the control process did not start: exit {code}"));
        self.wait_until("the launcher attaches", || self.status_polls() > 0).await;
        (app, hold)
    }

    /// Whether an update landed: `_update\landed` written, or the exe `launchable` wrote replaced.
    fn landed_anything(&self) -> bool {
        self.layout().landed.exists() || std::fs::read(self.layout().exe).unwrap() != b"old exe"
    }

    /// The day's timeline as the exit leaves it on disk, for after the API has gone.
    fn timeline_on_disk(&self) -> Vec<String> {
        let file = self.layout().app_logs.join(format!("events-{}.jsonl", local_date(SystemTime::now())));
        let text = std::fs::read_to_string(file).unwrap_or_default();
        text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["message"].as_str().unwrap().to_string()).collect()
    }

    fn restart_encoder_calls(&self) -> usize {
        self.sup.calls().iter().filter(|c| matches!(c, Call::Control { cmd: ControlCmd::RestartEncoder, .. })).count()
    }

    async fn wait_for_server(&self) {
        for _ in 0..100 {
            if reqwest::get(self.url("/api/token")).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the control process never answered");
    }

    async fn wait_until(&self, what: &str, done: impl Fn() -> bool) {
        self.wait_for(Duration::from_secs(3), what, done).await;
    }

    async fn wait_for(&self, limit: Duration, what: &str, done: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < deadline {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting until {what}");
    }
}

/// Long enough for the launcher's three missed polls, 2 s apart, and its launch.
const RELAUNCH_WITHIN: Duration = Duration::from_secs(12);

/// Port 4417 as `pid_on_port` reports it: taken by PID 4242 until the instant `hold_for` sets.
#[derive(Default)]
struct PortHold(Arc<Mutex<Option<Instant>>>);

impl PortHold {
    fn hold_for(&self, d: Duration) {
        *self.0.lock().unwrap() = Some(Instant::now() + d);
    }
}

/// Past the launcher's three missed polls, 2 s apart, and the landing watch's polls, 2 s apart too.
const SLOW_STOP: Duration = Duration::from_secs(9);
/// The slow stop, the exit and some slack.
const SLOW_QUIT_EXITS_WITHIN: Duration = Duration::from_secs(20);

async fn exit_code_after_a_slow_quit(app: App) -> i32 {
    tokio::time::timeout(SLOW_QUIT_EXITS_WITHIN, app.wait()).await.expect("the control process exited")
}

async fn json_of(reply: reqwest::Response) -> Value {
    let text = reply.text().await.expect("the whole reply arrives");
    serde_json::from_str(&text).unwrap()
}

/// `app.wait()` with a bound, so a control process that never exits fails the test instead of hanging it.
async fn exit_code(app: App) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), app.wait()).await.expect("the control process exited")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_starts_nothing_until_a_playlist_is_saved() {
    // Arrange: the shipped example's placeholder playlist
    let rig = Rig::new("PLxxxxxxxxxxxxxxxx");
    let app = rig.start().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let polls_on_first_run = rig.status_polls();

    // Act
    let saved = rig.post("/api/config", json!({ "playlists": [{ "id": "PLrealplaylist001" }] })).await;
    rig.wait_until("the launcher polls the supervisor", || rig.status_polls() > 0).await;

    // Assert
    assert_eq!(saved.status(), 200);
    assert_eq!(polls_on_first_run, 0, "a first run must not start the launcher");
    assert!(rig.spawner.0.lock().unwrap().is_empty(), "the supervisor answers, so nothing is spawned");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saving_config_rebuilds_discord_and_twitch_but_keeps_the_restart_clock() {
    // Arrange: one restart through the first Twitch checker
    let rig = Rig::new("PLrealplaylist001");
    let app = rig.start().await;
    let before = app.twitch();
    before.handle_frozen().await;

    // Act
    let saved = rig.post("/api/config", json!({ "discord": { "webhookUrl": NEW_HOOK } })).await;
    let after = app.twitch();
    after.handle_frozen().await;

    // Assert
    assert_eq!(saved.status(), 200);
    assert!(!Arc::ptr_eq(&before, &after), "the save builds a new Twitch checker");
    assert_eq!(rig.restart_encoder_calls(), 1, "the second freeze falls inside the shared 30 min window");
    let hooks: Vec<String> = rig.http.requests().iter().map(|r| r.url.clone()).collect();
    assert!(hooks.iter().any(|u| u == NEW_HOOK), "the window's critical goes to the new webhook: {hooks:?}");
    assert!(!hooks.iter().any(|u| u == OLD_HOOK), "nothing goes to the old webhook: {hooks:?}");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_port_returns_2_with_the_message() {
    // Arrange: another program holds the port
    let rig = Rig::new("PLrealplaylist001");
    let _held = std::net::TcpListener::bind(("127.0.0.1", rig.port)).unwrap();

    // Act
    let code = app::serve(rig.wiring()).await;

    // Assert
    assert_eq!(code, 2);
    let message = format!("Port {} is in use. Is another StreamLoop running? Close it and start this one again.", rig.port);
    assert!(captured_logs().text().contains(&message), "the log says why");
    assert_eq!(rig.status_polls(), 0, "nothing starts without the port");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removes_the_3_0_leftovers_and_nothing_else() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    let root = rig.root.clone();
    let file = |rel: &str| {
        let p = rel.split('/').fold(root.clone(), |p, part| p.join(part));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x").unwrap();
    };
    let leftovers = [
        "app/src/server/index.ts",
        "app/node_modules/tsx/package.json",
        "app/package.json",
        "app/package-lock.json",
        "app/tsconfig.json",
        "app/node-version.txt",
        "supervisor/streamloop-supervisor.exe",
        "_update_tmp/app/package.json",
        "_update_old/src/server/index.ts",
        "START.bat.new",
        "_apply_launcher.bat",
        "_update_old_node/node.exe",
        "_update_bad_supervisor/streamloop-supervisor.exe",
        "_update_bad_app/package.json",
        "data/update-swapping",
        "data/update-checking",
        "data/update-swap.log",
    ];
    let keepers = [
        "app/logs/streamloop-2026-01-01.log",
        "app/config.example.json",
        "data/state.json",
        "data/update-state.json",
        "supervisor/slate.mp4",
        "_update/streamloop.exe",
        "streamloop.exe",
        "START.bat",
    ];
    for rel in leftovers.iter().chain(keepers.iter()) {
        file(rel);
    }
    std::fs::write(rig.layout().update_state, r#"{"result":"ok"}"#).unwrap();

    // Act
    let app = rig.start().await;

    // Assert
    let at = |rel: &str| rel.split('/').fold(root.clone(), |p, part| p.join(part));
    for gone in ["app/src", "app/node_modules", "app/package.json", "app/package-lock.json", "app/tsconfig.json", "app/node-version.txt"] {
        assert!(!at(gone).exists(), "{gone} is a 3.0 leftover");
    }
    for gone in ["supervisor/streamloop-supervisor.exe", "_update_tmp", "_update_old", "START.bat.new", "_apply_launcher.bat"] {
        assert!(!at(gone).exists(), "{gone} is a 3.0 leftover");
    }
    for gone in ["_update_old_node", "_update_bad_supervisor", "_update_bad_app", "data/update-swapping", "data/update-checking", "data/update-swap.log"] {
        assert!(!at(gone).exists(), "{gone} is a leftover of a 3.0 full update");
    }
    for kept in keepers.iter().chain(["app/config.json", "app/logs", "data"].iter()) {
        assert!(at(kept).exists(), "{kept} must stay");
    }
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_replaces_this_installs_vbs_with_the_run_value() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    let bat = rig.root.join("START.bat");
    let text = format!("Set WshShell = CreateObject(\"WScript.Shell\")\r\nWshShell.Run \"\"\"{}\"\"\", 1, False", bat.display());
    let vbs = rig.root.join("startup").join("StreamLoop.vbs");
    std::fs::create_dir_all(vbs.parent().unwrap()).unwrap();
    std::fs::write(&vbs, text).unwrap();
    // Act
    let app = rig.start().await;
    // Assert
    let value = rig.autostart.run_value().unwrap().expect("the Run value is written");
    assert!(value.ends_with("streamloop.exe\" ui --tray"), "{value}");
    assert!(!vbs.exists());
    assert!(captured_logs().text().contains("Autostart moved from StreamLoop.vbs to the Run entry"));
    drop(app);
}

fn install_with(files: &[&str]) -> (tempfile::TempDir, Layout) {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(&dir.path().join("root"));
    for rel in files {
        let p = rel.split('/').fold(layout.root.clone(), |p, part| p.join(part));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x").unwrap();
    }
    (dir, layout)
}

#[test]
fn removes_pot_and_node_once_the_rust_token_server_is_installed() {
    // Arrange
    let (_dir, layout) = install_with(&["supervisor/streamloop-pot.exe", "pot/build/main.js", "pot/node_modules/canvas/package.json", "node/node.exe", "node/node_modules/npm/package.json"]);

    // Act
    remove_3_0_leftovers(&layout);

    // Assert
    assert!(!layout.root.join("pot").exists(), "pot\\ goes");
    assert!(!layout.root.join("node").exists(), "node\\ goes");
    assert!(layout.token_server.exists(), "the token server stays");
}

#[test]
fn keeps_pot_and_node_while_the_install_still_needs_them() {
    // Arrange: an exe-only update from before the Rust token server: the fallback pair is all the install has
    let (_dir, layout) = install_with(&["pot/build/main.js", "pot/node_modules/canvas/package.json", "node/node.exe"]);

    // Act
    remove_3_0_leftovers(&layout);

    // Assert
    assert!(layout.root.join("pot").join("build").join("main.js").exists());
    assert!(layout.root.join("node").join("node.exe").exists());
}

#[cfg(windows)]
#[test]
fn retries_a_partly_removed_pot_and_node_at_the_next_start() {
    // Arrange: a first pass that left files behind, as a held file would
    let (_dir, layout) = install_with(&["supervisor/streamloop-pot.exe", "pot/build/main.js", "node/node.exe"]);
    let held = layout.root.join("node").join("node.exe");
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::File::options().read(true).share_mode(1).open(&held).unwrap();
    remove_3_0_leftovers(&layout);
    let first_pass_kept_it = held.exists();
    drop(file);

    // Act
    remove_3_0_leftovers(&layout);

    // Assert
    assert!(first_pass_kept_it, "a held file survives the pass that met it");
    assert!(!layout.root.join("node").exists() && !layout.root.join("pot").exists());
}

#[cfg(windows)]
#[test]
fn keeps_pot_whole_while_a_running_fallback_holds_node_exe() {
    // Arrange: the TypeScript server still runs, so node.exe is held and its scripts in pot\ are still needed
    let (_dir, layout) = install_with(&["supervisor/streamloop-pot.exe", "pot/build/main.js", "pot/node_modules/canvas/package.json", "node/node.exe"]);
    let held = layout.root.join("node").join("node.exe");
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::File::options().read(true).share_mode(1).open(&held).unwrap();

    // Act
    remove_3_0_leftovers(&layout);
    drop(file);

    // Assert
    assert!(layout.root.join("pot").join("build").join("main.js").exists(), "pot\\build\\main.js stays");
    assert!(layout.root.join("pot").join("node_modules").join("canvas").join("package.json").exists(), "pot\\node_modules stays");
}

#[cfg(windows)]
#[test]
fn a_node_modules_junction_goes_and_its_target_stays() {
    // Arrange: app\node_modules as a junction to a folder outside the install, as a dev root has it
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(&dir.path().join("root"));
    let target = dir.path().join("repo-node_modules");
    std::fs::create_dir_all(target.join("tsx")).unwrap();
    std::fs::write(target.join("tsx").join("package.json"), "{}").unwrap();
    std::fs::create_dir_all(&layout.app_dir).unwrap();
    let link = layout.app_dir.join("node_modules");
    let made = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&link).arg(&target).output().unwrap();
    assert!(made.status.success(), "mklink /J failed");

    // Act
    remove_3_0_leftovers(&layout);

    // Assert
    assert!(std::fs::symlink_metadata(&link).is_err(), "the junction is gone");
    assert!(target.join("tsx").join("package.json").exists(), "what it pointed at stays");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_75_hook_returns_75() {
    // Arrange: the control process in a runtime of its own, ended as `run` ends it, as the process exits
    let mut rig = Rig::new("PLrealplaylist001");
    rig.vars.test_hooks = true;
    let wiring = rig.wiring();
    let process = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let code = runtime.block_on(app::serve(wiring));
        runtime.shutdown_background();
        code
    });
    rig.wait_for_server().await;

    // Act
    let reply = rig.post("/api/test/exit-75", json!({})).await;
    let code = tokio::task::spawn_blocking(move || process.join().unwrap()).await.unwrap();
    let status = reply.status();
    let body = json_of(reply).await;

    // Assert
    assert_eq!(code, 75);
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "ok": true }), "the reply went out before the exit");
}

#[tokio::test]
async fn a_second_exit_request_changes_nothing() {
    // Arrange: two Update Now clicks that both reach the restart after one landing
    let exit = ExitSwitch::new();

    // Act
    let first = exit.request(75);
    let second = exit.request(75);
    let shutdown_after = exit.request(0);

    // Assert
    assert!(first);
    assert!(!second, "a second restart is a no-op");
    assert!(!shutdown_after, "nor does a later shutdown change the exit");
    assert_eq!(exit.wait().await, 75);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_launch_guard_fallback_is_saved_in_the_config_the_dashboard_reads() {
    // Arrange: no supervisor answers, h264_nvenc fails twice and h264_amf works
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.set_status(None);
    std::fs::create_dir_all(rig.root.join("ffmpeg")).unwrap();
    std::fs::write(rig.layout().ffmpeg, "").unwrap();
    std::fs::write(rig.layout().exe, "").unwrap();
    let failed = RunOutput { code: Some(1), ..RunOutput::default() };
    rig.runner.push(Ok(failed.clone()));
    rig.runner.push(Ok(failed));
    rig.runner.push(Ok(RunOutput::ok("")));
    let app = rig.start().await;

    // Act: Start launches at once, through the guard
    let started = rig.post("/api/stream/start", json!({})).await;
    rig.wait_until("the guard lets the launch through", || !rig.spawner.0.lock().unwrap().is_empty()).await;
    let shown = json_of(reqwest::get(rig.url("/api/config")).await.unwrap()).await;

    // Assert
    assert_eq!(started.status(), 200);
    assert_eq!(shown["supervisor"]["encoder"], "h264_amf", "the dashboard reads the fallback the guard saved");
    assert_eq!(shown["supervisor"]["preferredEncoder"], "h264_nvenc");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(rig.layout().config).unwrap()).unwrap();
    assert_eq!(saved["supervisor"]["encoder"], "h264_amf");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_youtube_pause_updates_yt_dlp() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    rig.patch_config(json!({ "autoUpdateYtdlp": true }));
    rig.write("yt-dlp/yt-dlp.exe", b"");
    for out in ["", "2026.11.22.232654\n", "", "2026.11.23.223556\n"] {
        rig.runner.push(Ok(RunOutput::ok(out)));
    }
    let updates = || rig.runner.calls().iter().filter(|c| c.args == ["-U"]).count();
    let app = rig.start().await;
    rig.wait_until("the update at start", || updates() == 1).await;
    let pause = json!({
        "ts": "2026-11-23T20:08:27.478Z",
        "level": "critical",
        "kind": "youtube-breaker-open",
        "reason": "ERROR: [youtube] ki6bxoUh44w: Requested format is not available",
        "bot": false
    });

    // Act
    rig.write(&format!("data/logs/supervisor-events-{}.jsonl", local_date(SystemTime::now())), format!("{pause}\n").as_bytes());

    // Assert
    rig.wait_for(Duration::from_secs(6), "an update while YouTube lookups are paused", || updates() == 2).await;
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_returns_0_and_never_stops_the_supervisor() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    let app = rig.start().await;
    rig.wait_until("the launcher attaches", || rig.status_polls() > 0).await;

    // Act
    let reply = rig.post("/api/shutdown", json!({})).await;
    let code = exit_code(app).await;

    // Assert
    assert_eq!(reply.status(), 200);
    assert_eq!(code, 0);
    assert!(!rig.sup.calls().contains(&Call::Shutdown), "no /shutdown reaches the supervisor");
}

/// Starts the control process with a supervisor that stops at `/shutdown`, quits it once the launcher has
/// attached, and returns the exit code.
async fn quit_and_exit(rig: &Rig) -> i32 {
    rig.sup.stop_answering_on_shutdown();
    let app = rig.start().await;
    rig.wait_until("the launcher attaches", || rig.status_polls() > 0).await;
    let reply = rig.post("/api/quit", json!({})).await;
    assert_eq!(reply.status(), 200);
    exit_code(app).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_stops_the_supervisor_and_exits_0() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.stop_answering_on_shutdown();
    let app = rig.start().await;
    rig.wait_until("the launcher attaches", || rig.status_polls() > 0).await;

    // Act
    let reply = rig.post("/api/quit", json!({})).await;
    let status = reply.status();
    let body = json_of(reply).await;
    let code = exit_code(app).await;

    // Assert
    assert_eq!((status, body), (reqwest::StatusCode::OK, json!({ "ok": true })));
    assert_eq!(code, 0);
    assert_eq!(rig.supervisor_shutdowns(), 1, "the supervisor got POST /shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_writes_no_stopped_by_user_marker() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");

    // Act
    let code = quit_and_exit(&rig).await;

    // Assert
    assert_eq!(code, 0);
    assert!(!rig.layout().stopped_marker.exists(), "the next start follows autoStartStream");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_keeps_an_earlier_marker() {
    // Arrange: stopped from the dashboard before, so no supervisor runs
    let rig = Rig::new("PLrealplaylist001");
    rig.write("data/stopped-by-user", b"");
    rig.sup.set_status(None);
    let app = rig.start().await;

    // Act
    let reply = rig.post("/api/quit", json!({})).await;
    let code = exit_code(app).await;

    // Assert
    assert_eq!(reply.status(), 200);
    assert_eq!(code, 0);
    assert!(rig.layout().stopped_marker.exists(), "Stop then Quit stays stopped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_holds_the_launcher_so_nothing_relaunches() {
    // Arrange: a supervisor that stops answering at /shutdown but holds port 4417 for 9 s more, past the
    // launcher's three missed polls, and a launch that would spawn
    let rig = Rig::new("PLrealplaylist001");
    rig.launchable();
    let (app, port) = rig.start_with_port_hold().await;

    // Act
    port.hold_for(SLOW_STOP);
    let reply = rig.post("/api/quit", json!({})).await;
    let code = exit_code_after_a_slow_quit(app).await;

    // Assert
    assert_eq!(reply.status(), 200);
    assert_eq!(code, 0);
    assert_eq!(rig.supervisor_shutdowns(), 1);
    assert_eq!(rig.spawns(), 0, "nothing was launched while the supervisor went away");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_during_the_quit_launches_nothing() {
    // Arrange: the quit's stop lasts 9 s, and a launch would spawn
    let rig = Rig::new("PLrealplaylist001");
    rig.launchable();
    let (app, port) = rig.start_with_port_hold().await;
    port.hold_for(SLOW_STOP);
    let quit = rig.post("/api/quit", json!({})).await;
    rig.wait_until("the quit stops the supervisor", || rig.supervisor_shutdowns() == 1).await;

    // Act
    let start = rig.post("/api/stream/start", json!({})).await;
    let code = exit_code_after_a_slow_quit(app).await;

    // Assert
    assert_eq!((quit.status(), start.status()), (reqwest::StatusCode::OK, reqwest::StatusCode::OK), "Start keeps its usual reply");
    assert_eq!(code, 0);
    assert_eq!(rig.spawns(), 0, "no supervisor outlives the control process");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_during_the_quit_keeps_an_earlier_marker() {
    // Arrange: stopped from the dashboard before, the quit's stop lasting 9 s, and a launch that would spawn
    let rig = Rig::new("PLrealplaylist001");
    rig.write("data/stopped-by-user", b"");
    rig.sup.set_status(None);
    rig.launchable();
    let (app, port) = rig.start_with_port_hold().await;
    port.hold_for(SLOW_STOP);
    let quit = rig.post("/api/quit", json!({})).await;
    rig.wait_until("the quit stops the supervisor", || rig.supervisor_shutdowns() == 1).await;

    // Act
    let start = rig.post("/api/stream/start", json!({})).await;
    let code = exit_code_after_a_slow_quit(app).await;

    // Assert
    assert_eq!((quit.status(), start.status()), (reqwest::StatusCode::OK, reqwest::StatusCode::OK));
    assert_eq!(code, 0);
    assert!(rig.layout().stopped_marker.exists(), "the next start stays stopped");
    assert_eq!(rig.spawns(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_now_right_after_the_quit_answers_lands_nothing() {
    // Arrange: the new exe staged, the quit's stop lasting 9 s, and a launch that would spawn
    let mut rig = Rig::new("PLrealplaylist001");
    rig.offer_new_version();
    rig.runner.push(Ok(RunOutput::ok(&format!("{NEW_VERSION}\r\n"))));
    rig.launchable();
    let (app, port) = rig.start_with_port_hold().await;
    rig.post("/api/update/check", json!({})).await;
    rig.wait_until("the exe is staged", || rig.layout().staged_exe.exists()).await;
    port.hold_for(SLOW_STOP);

    // Act: within the half second before the quit acts
    let quit = rig.post("/api/quit", json!({})).await;
    let applied = rig.post("/api/update/apply", json!({})).await;
    let code = exit_code_after_a_slow_quit(app).await;

    // Assert
    assert_eq!((quit.status(), applied.status()), (reqwest::StatusCode::OK, reqwest::StatusCode::OK), "Update Now keeps its usual reply");
    assert_eq!(code, 0);
    assert!(!rig.landed_anything());
    assert_eq!(rig.spawns(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_during_the_quit_lands_nothing() {
    // Arrange: the new exe staged with autoUpdateCheck on, so a dashboard Stop would land it, and the quit's
    // stop lasting 9 s
    let mut rig = Rig::new("PLrealplaylist001");
    rig.patch_config(json!({ "autoUpdateCheck": true }));
    rig.offer_new_version();
    rig.runner.push(Ok(RunOutput::ok(&format!("{NEW_VERSION}\r\n"))));
    rig.launchable();
    let (app, port) = rig.start_with_port_hold().await;
    rig.post("/api/update/check", json!({})).await;
    rig.wait_until("the exe is staged", || rig.layout().staged_exe.exists()).await;
    port.hold_for(SLOW_STOP);
    let quit = rig.post("/api/quit", json!({})).await;
    rig.wait_until("the quit stops the supervisor", || rig.supervisor_shutdowns() == 1).await;

    // Act
    let stop = rig.post("/api/stream/stop", json!({})).await;
    let code = exit_code_after_a_slow_quit(app).await;

    // Assert
    assert_eq!((quit.status(), stop.status()), (reqwest::StatusCode::OK, reqwest::StatusCode::OK));
    assert_eq!(code, 0);
    assert!(!rig.landed_anything());
    assert_eq!(rig.spawns(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_puts_its_line_on_the_timeline_and_on_discord_as_info() {
    // Arrange: the recovery toggle on, as by default, and off
    let on = Rig::new("PLrealplaylist001");
    let off = Rig::new("PLrealplaylist001");
    off.patch_config(json!({ "discord": { "webhookUrl": OLD_HOOK, "events": { "recovery": false } } }));

    // Act
    let codes = [quit_and_exit(&on).await, quit_and_exit(&off).await];

    // Assert
    assert_eq!(codes, [0, 0]);
    for rig in [&on, &off] {
        assert!(rig.timeline_on_disk().contains(&QUIT_LINE.to_string()), "{:?}", rig.timeline_on_disk());
    }
    let sent: Vec<Value> = on.discord_posts().iter().map(|p| serde_json::from_str(p).unwrap()).filter(|p: &Value| p.to_string().contains(QUIT_LINE)).collect();
    assert_eq!(sent.len(), 1, "{:?}", on.discord_posts());
    assert_eq!(sent[0]["embeds"][0]["color"], 3_447_003, "sent as info");
    assert!(!off.discord_posts().iter().any(|p| p.contains(QUIT_LINE)), "{:?}", off.discord_posts());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quit_asked_twice_runs_once() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.stop_answering_on_shutdown();
    let app = rig.start().await;
    rig.wait_until("the launcher attaches", || rig.status_polls() > 0).await;

    // Act
    let first = rig.post("/api/quit", json!({})).await;
    let second = rig.post("/api/quit", json!({})).await;
    let code = exit_code(app).await;

    // Assert
    assert_eq!((first.status(), second.status()), (reqwest::StatusCode::OK, reqwest::StatusCode::OK));
    assert_eq!(code, 0);
    assert_eq!(rig.supervisor_shutdowns(), 1);
    assert_eq!(rig.timeline_on_disk().iter().filter(|m| *m == QUIT_LINE).count(), 1);
}

/// A `launch` helper whose supervisor starts answering `after` the spawn. The helper exits then, as `launch`
/// exits once the supervisor's port answers.
struct LateSupervisor {
    sup: Arc<FakeSupervisor>,
    after: Duration,
    spawned: Mutex<Option<Instant>>,
}

struct HandsOff(tokio::sync::oneshot::Receiver<()>);

#[async_trait]
impl SpawnHandle for HandsOff {
    async fn exit_code(self: Box<Self>) -> Option<i32> {
        let _ = self.0.await;
        Some(0)
    }
}

impl Spawner for LateSupervisor {
    fn spawn(&self, _exe: &Path, _args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>> {
        *self.spawned.lock().unwrap() = Some(Instant::now());
        let (up, handed_off) = tokio::sync::oneshot::channel();
        let (sup, after) = (self.sup.clone(), self.after);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            sup.set_status(Some(playing()));
            let _ = up.send(());
        });
        Ok(Box::new(HandsOff(handed_off)))
    }
}

async fn sleep_until(at: Instant) {
    tokio::time::sleep(at.saturating_duration_since(Instant::now())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quit_right_after_a_launch_stops_the_supervisor_that_launch_brings_up() {
    // Arrange: nothing answers, so the launcher launches a supervisor that answers 1 s after the spawn
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.set_status(None);
    rig.sup.stop_answering_on_shutdown();
    rig.launchable();
    let spawner = Arc::new(LateSupervisor { sup: rig.sup.clone(), after: Duration::from_secs(1), spawned: Mutex::new(None) });
    let mut wiring = rig.wiring();
    wiring.spawner = spawner.clone();
    let app = App::start(wiring).await.unwrap_or_else(|code| panic!("the control process did not start: exit {code}"));
    rig.wait_for(RELAUNCH_WITHIN, "the launcher spawns a supervisor", || spawner.spawned.lock().unwrap().is_some()).await;
    let spawned = spawner.spawned.lock().unwrap().expect("spawned");
    sleep_until(spawned + Duration::from_millis(100)).await;

    // Act
    let reply = rig.post("/api/quit", json!({})).await;
    let code = exit_code(app).await;
    sleep_until(spawned + Duration::from_millis(1500)).await;

    // Assert
    assert_eq!(reply.status(), 200);
    assert_eq!(code, 0);
    assert_eq!(rig.supervisor_shutdowns(), 1);
    assert!(rig.sup.status().await.is_none(), "the supervisor that came up after the quit began was stopped before the exit");
}

#[test]
fn an_unknown_argument_returns_64() {
    // Arrange
    let args = vec!["--bogus".to_string()];

    // Act
    let code = app::run(args);

    // Assert
    assert_eq!(code, 64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_landing_is_confirmed_by_the_check_and_stays_held() {
    // Arrange: landed while the stream waited for Start, no supervisor, and a launch would spawn
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.set_status(None);
    rig.landed(VERSION, false, true);
    rig.runner.push(Ok(RunOutput::ok("ok")));
    rig.launchable();

    // Act
    let app = rig.start().await;
    rig.wait_until("the update is confirmed", || !rig.layout().landed.exists()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert
    assert_eq!(rig.runner.calls()[0].args, ["check", "--root", &rig.root.to_string_lossy()], "the check confirmed it");
    assert!(rig.spawner.0.lock().unwrap().is_empty(), "nothing is launched");
    assert!(rig.layout().waiting_for_start.exists(), "the stream still waits for Start");
    assert_eq!(rig.get("/api/status").await["launcher"], "ready");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_on_air_landing_is_not_held_once_confirmed() {
    // Arrange: the new supervisor answers with this version
    let rig = Rig::new("PLrealplaylist001");
    rig.landed(VERSION, false, false);

    // Act
    let app = rig.start().await;
    rig.wait_until("the update is confirmed", || !rig.layout().landed.exists()).await;

    // Assert
    assert!(!rig.layout().waiting_for_start.exists());
    assert_eq!(rig.get("/api/status").await["launcher"], "running");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_landing_that_never_swapped_relaunches_the_supervisor() {
    // Arrange: a landing of another version cut off before its renames, after the supervisor had stopped
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.set_status(None);
    rig.landed(NEW_VERSION, false, false);
    rig.launchable();

    // Act
    let app = rig.start().await;
    rig.wait_for(RELAUNCH_WITHIN, "the supervisor is launched", || rig.spawns() > 0).await;

    // Assert
    assert!(!rig.layout().landed.exists());
    assert!(!rig.layout().waiting_for_start.exists());
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn during_a_confirmation_update_now_is_refused_and_a_check_stages_nothing() {
    // Arrange: an install with a newer release offered, and a confirmation that waits for a supervisor
    let mut rig = Rig::new("PLrealplaylist001");
    rig.offer_new_version();
    rig.sup.set_status(None);
    rig.landed(VERSION, true, false);
    let app = rig.start().await;

    // Act
    let applied = rig.post("/api/update/apply", json!({})).await;
    let applied_status = applied.status();
    let applied = json_of(applied).await;
    let checked = json_of(rig.post("/api/update/check", json!({})).await).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Assert
    assert_eq!(applied_status, 409);
    assert_eq!(applied, json!({ "error": "An update is running. Try again when it is done." }));
    assert_eq!((checked["updateAvailable"].clone(), checked["latestVersion"].clone()), (json!(false), Value::Null), "the check did not run");
    assert!(rig.updater_http.requests().is_empty(), "nothing was downloaded");
    assert!(!rig.layout().staged_exe.exists());
    assert!(rig.layout().landed.exists(), "still confirming");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_now_on_a_first_run_installs_without_starting_a_supervisor() {
    // Arrange: an install with only the example's playlist, the new exe staged, and a launch that would spawn
    let mut rig = Rig::new("PLxxxxxxxxxxxxxxxx");
    rig.offer_new_version();
    rig.sup.set_status(None);
    rig.runner.push(Ok(RunOutput::ok(&format!("{NEW_VERSION}\r\n"))));
    rig.launchable();
    let app = rig.start().await;
    let checked = json_of(rig.post("/api/update/check", json!({})).await).await;
    rig.wait_until("the exe is staged", || rig.layout().staged_exe.exists()).await;

    // Act
    let applied = rig.post("/api/update/apply", json!({})).await;
    let code = exit_code(app).await;

    // Assert
    assert_eq!(checked["updateAvailable"], true);
    assert_eq!(applied.status(), 200);
    assert_eq!(code, 75);
    assert_eq!(std::fs::read(rig.layout().exe).unwrap(), NEW_EXE.as_bytes());
    let record = LandedRecord::read(&rig.layout().landed).expect("the landing left its record");
    assert!(record.held, "a first run counts as held, so the next start confirms with the check");
    assert!(rig.spawner.0.lock().unwrap().is_empty(), "no supervisor is started");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ctrl_c_during_the_start_ends_the_process_with_0_once_it_is_up() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    rig.stop.send(()).unwrap();

    // Act
    let app = rig.start().await;
    let code = exit_code(app).await;

    // Assert
    assert_eq!(code, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ctrl_c_after_a_restart_was_asked_keeps_75() {
    // Arrange
    let mut rig = Rig::new("PLrealplaylist001");
    rig.vars.test_hooks = true;
    let app = rig.start().await;
    let reply = rig.post("/api/test/exit-75", json!({})).await;

    // Act
    rig.stop.send(()).unwrap();
    let code = exit_code(app).await;

    // Assert
    assert_eq!(reply.status(), 200);
    assert_eq!(code, 75);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollback_by_start_bat_goes_to_the_timeline_and_out_once_as_a_critical() {
    // Arrange: START.bat put this exe back and moved the record, leaving the progress at the check
    let rig = Rig::new("PLrealplaylist001");
    let progress = json!({
        "from": VERSION, "to": NEW_VERSION, "parts": [], "folders": [], "step": "check", "result": "running", "reason": null,
        "startedAt": "2026-11-10T10:00:00.000Z", "stepStartedAt": "2026-11-10T10:00:20.000Z", "baseline": null
    });
    rig.write("data/update-state.json", progress.to_string().as_bytes());
    rig.rolled_back(false);
    let text = format!("The update to {NEW_VERSION} failed at checking the new version: The updated StreamLoop stopped before it was confirmed. Still on {VERSION}.");

    // Act
    let app = rig.start().await;
    rig.wait_until("the critical is sent", || !rig.discord_posts().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert
    assert!(rig.timeline().await.contains(&text), "{:?}", rig.timeline().await);
    let posts = rig.discord_posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(posts[0].contains(&text) && posts[0].contains("CRITICAL"), "{}", posts[0]);
    let state: Value = serde_json::from_str(&std::fs::read_to_string(rig.layout().update_state).unwrap()).unwrap();
    assert_eq!(state["result"], "rolled-back");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_landed_start_that_was_not_held_relaunches_a_supervisor_that_is_down() {
    // Arrange: an on-air landing whose one launch did not bring a supervisor up
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.set_status(None);
    rig.landed(VERSION, true, false);
    rig.launchable();

    // Act
    let app = rig.start().await;
    let held_at_start = rig.layout().waiting_for_start.exists();
    rig.wait_for(RELAUNCH_WITHIN, "the launcher relaunches the supervisor", || rig.spawns() > 0).await;

    // Assert
    assert!(!held_at_start, "a landing that was on air does not wait for Start");
    assert!(rig.layout().landed.exists(), "the confirmation still runs");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollback_by_start_bat_brings_an_on_air_stream_back_with_auto_start_stream_off() {
    // Arrange
    let mut rig = Rig::new("PLrealplaylist001");
    rig.patch_config(json!({ "autoStartStream": false }));
    rig.vars.start = Some("rollback".into());
    rig.rolled_back(false);
    rig.sup.set_status(None);
    rig.launchable();

    // Act
    let app = rig.start().await;
    let held_at_start = rig.layout().waiting_for_start.exists();
    rig.wait_for(RELAUNCH_WITHIN, "the launcher relaunches the supervisor", || rig.spawns() > 0).await;

    // Assert
    assert!(!held_at_start, "the stream was on air before the update");
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rollback_by_start_bat_keeps_a_held_stream_held_with_auto_start_stream_on() {
    // Arrange
    let mut rig = Rig::new("PLrealplaylist001");
    rig.vars.start = Some("rollback".into());
    rig.rolled_back(true);
    rig.sup.set_status(None);
    rig.launchable();

    // Act
    let app = rig.start().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert
    assert!(rig.layout().waiting_for_start.exists(), "the stream waited for Start before the update");
    assert_eq!(rig.get("/api/status").await["launcher"], "ready");
    assert_eq!(rig.spawns(), 0);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn during_a_confirmation_stop_is_let_through_and_the_check_confirms_the_update() {
    // Arrange: an on-air landing whose new supervisor has not answered yet
    let rig = Rig::new("PLrealplaylist001");
    rig.sup.set_status(None);
    rig.landed(VERSION, true, false);
    rig.runner.push(Ok(RunOutput::ok("ok")));
    let app = rig.start().await;

    // Act
    let stop = rig.post("/api/stream/stop", json!({})).await;
    let stop_status = stop.status();
    let start = rig.post("/api/stream/start", json!({})).await;
    let start_status = start.status();
    let start = json_of(start).await;
    rig.wait_for(Duration::from_secs(6), "the update is confirmed", || !rig.layout().landed.exists()).await;

    // Assert
    assert_eq!(stop_status, 200);
    assert_eq!((start_status, start), (reqwest::StatusCode::CONFLICT, json!({ "error": "An update is running. Try again when it is done." })));
    assert!(rig.sup.calls().contains(&Call::Shutdown));
    assert!(rig.layout().stopped_marker.exists());
    assert_eq!(rig.runner.calls()[0].args, ["check", "--root", &rig.root.to_string_lossy()], "the check confirmed it");
    assert_eq!(rig.spawns(), 0);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ctrl_c_during_a_landing_exits_once_the_new_supervisor_is_launched() {
    // Arrange: the new exe staged, then Update Now with the supervisor gone and a launch helper that has not handed off
    let mut rig = Rig::new("PLrealplaylist001");
    rig.offer_new_version();
    rig.runner.push(Ok(RunOutput::ok(&format!("{NEW_VERSION}\r\n"))));
    rig.launchable();
    let gate = rig.hold_launches();
    let app = rig.start().await;
    rig.post("/api/update/check", json!({})).await;
    rig.wait_until("the exe is staged", || rig.layout().staged_exe.exists()).await;
    rig.sup.set_status(None);
    let applied = rig.post("/api/update/apply", json!({})).await;
    rig.wait_until("the new exe is launched", || rig.spawns() > 0).await;

    // Act
    rig.stop.send(()).unwrap();
    let wait = app.wait();
    tokio::pin!(wait);
    let before_hand_off = tokio::time::timeout(Duration::from_millis(700), &mut wait).await;
    let exited_early = before_hand_off.is_ok();
    gate.add_permits(1);
    let code = match before_hand_off {
        Ok(code) => code,
        Err(_) => tokio::time::timeout(Duration::from_secs(10), &mut wait).await.expect("the control process exited"),
    };

    // Assert
    assert_eq!(applied.status(), 200);
    assert!(!exited_early, "the exit did not wait for the landing's launch");
    assert_eq!(code, 0);
    assert_eq!(std::fs::read(rig.layout().exe).unwrap(), NEW_EXE.as_bytes());
    assert!(rig.layout().landed.exists(), "the next start confirms the update");
}

fn kinds() -> [StartKind; 5] {
    [StartKind::First, StartKind::Restart, StartKind::Update { held: false }, StartKind::Rollback { held: None }, StartKind::Rollback { held: Some(true) }]
}

#[test]
fn a_first_start_by_hand_opens_the_window() {
    // Arrange: START.bat's `first` word, no update landed, the app did not start it
    let (word, kind, from_app) = (Some("first"), StartKind::First, false);

    // Act
    let opens = opens_window(word, &kind, from_app);

    // Assert
    assert!(opens);
}

#[test]
fn a_first_start_from_the_app_does_not() {
    // Arrange: the app started this START.bat, so it has its window already
    let (word, kind, from_app) = (Some("first"), StartKind::First, true);

    // Act
    let opens = opens_window(word, &kind, from_app);

    // Assert
    assert!(!opens);
}

#[test]
fn a_start_without_start_bats_word_opens_nothing() {
    // Arrange: dev and the harness run `control --root` with no word, whatever kind the start is taken for
    for kind in kinds() {
        // Act
        let opens = opens_window(None, &kind, false);

        // Assert
        assert!(!opens, "{kind:?}");
    }
}

#[test]
fn restart_update_and_rollback_starts_never_do() {
    // Arrange: every kind but First, under each word START.bat sets
    for kind in kinds().into_iter().filter(|k| *k != StartKind::First) {
        for word in [Some("first"), Some("restart"), Some("rollback")] {
            // Act
            let opens = opens_window(word, &kind, false);

            // Assert
            assert!(!opens, "{word:?} {kind:?}");
        }
    }
    assert!(!opens_window(Some("restart"), &StartKind::First, false), "an unknown pairing stays shut");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_start_by_hand_asks_for_the_window_once() {
    // Arrange
    let mut rig = Rig::new("PLrealplaylist001");
    rig.vars.start = Some("first".into());

    // Act
    let app = rig.start().await;

    // Assert
    assert_eq!(rig.window_opens.load(Ordering::SeqCst), 1);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_with_no_playlist_still_asks_for_the_window() {
    // Arrange: the shipped example's placeholder playlist, so the wizard is what the window shows
    let mut rig = Rig::new("PLxxxxxxxxxxxxxxxx");
    rig.vars.start = Some("first".into());

    // Act
    let app = rig.start().await;

    // Assert
    assert_eq!(rig.window_opens.load(Ordering::SeqCst), 1);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_start_from_the_app_asks_for_no_window() {
    // Arrange
    let mut rig = Rig::new("PLrealplaylist001");
    rig.vars.start = Some("first".into());
    rig.vars.from_app = true;

    // Act
    let app = rig.start().await;

    // Assert
    assert_eq!(rig.window_opens.load(Ordering::SeqCst), 0);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_landed_update_asks_for_no_window_on_a_first_word() {
    // Arrange: START.bat's word says first, but an update landed
    let mut rig = Rig::new("PLrealplaylist001");
    rig.vars.start = Some("first".into());
    rig.landed(VERSION, true, false);

    // Act
    let app = rig.start().await;

    // Assert
    assert_eq!(rig.window_opens.load(Ordering::SeqCst), 0);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_open_admin_is_no_longer_read() {
    // Arrange: the old setting on, and a restart start
    let mut rig = Rig::new("PLrealplaylist001");
    rig.patch_config(json!({ "autoOpenAdmin": true }));
    rig.vars.start = Some("restart".into());

    // Act
    let app = rig.start().await;

    // Assert
    assert_eq!(rig.window_opens.load(Ordering::SeqCst), 0);
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_with_auto_open_admin_still_loads_and_keeps_the_key() {
    // Arrange
    let rig = Rig::new("PLrealplaylist001");
    rig.patch_config(json!({ "autoOpenAdmin": true }));
    let app = rig.start().await;

    // Act
    let saved = rig.post("/api/config", json!({ "discord": { "webhookUrl": NEW_HOOK } })).await;

    // Assert
    assert_eq!(saved.status(), 200);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(rig.layout().config).unwrap()).unwrap();
    assert_eq!(file["autoOpenAdmin"], json!(true));
    drop(app);
}
