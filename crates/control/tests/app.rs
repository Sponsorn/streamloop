//! The control process as `streamloop.exe [--root DIR]` wires it: start order, a config save,
//! the busy port, the 3.0 leftovers and the exit codes, on a real socket with fakes behind it.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use streamloop_common::api::{ControlCmd, SupervisorStatus};
use streamloop_common::paths::{Layout, Root};
use streamloop_common::version::VERSION;
use streamloop_control::app::{self, App, EnvVars, ExitSwitch, Wiring};
use streamloop_control::clock::SystemClock;
use streamloop_control::http::FakeHttp;
use streamloop_control::landing::LandedRecord;
use streamloop_control::launcher::{SpawnHandle, Spawner};
use streamloop_control::leftovers::remove_3_0_leftovers;
use streamloop_control::runner::{FakeRunner, RunOutput};
use streamloop_control::supervisor_client::fake::{Call, FakeSupervisor};
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
    vars: EnvVars,
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
            vars: EnvVars::default(),
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
            obs_running: Arc::new(|| false),
            kill_tree: Arc::new(|_| {}),
            pid_on_port: Arc::new(|_| None),
            protect: Arc::new(|_| Ok("blob".to_string())),
            setup_check: streamloop_control::api::setup_check_with(runner),
            open_admin: Arc::new(|_| {}),
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
