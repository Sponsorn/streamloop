//! The control process as `streamloop.exe [--root DIR]` wires it: start order, a config save,
//! the busy port, the 3.0 leftovers and the exit codes, on a real socket with fakes behind it.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use streamloop_common::api::{ControlCmd, SupervisorStatus};
use streamloop_common::paths::{Layout, Root};
use streamloop_common::version::VERSION;
use streamloop_control::app::{self, App, EnvVars, ExitSwitch, Wiring};
use streamloop_control::clock::SystemClock;
use streamloop_control::http::FakeHttp;
use streamloop_control::launcher::{SpawnHandle, Spawner};
use streamloop_control::runner::{FakeRunner, RunOutput};
use streamloop_control::supervisor_client::fake::{Call, FakeSupervisor};
use support::captured_logs;
use tempfile::TempDir;

const OLD_HOOK: &str = "https://discord.example/old";
const NEW_HOOK: &str = "https://discord.example/new";

/// Records each spawn and never starts a process.
#[derive(Default)]
struct NoSpawner(Mutex<Vec<Vec<String>>>);

struct Exited;

#[async_trait]
impl SpawnHandle for Exited {
    async fn exit_code(self: Box<Self>) -> Option<i32> {
        Some(0)
    }
}

impl Spawner for NoSpawner {
    fn spawn(&self, _exe: &Path, args: &[String]) -> std::io::Result<Box<dyn SpawnHandle>> {
        self.0.lock().unwrap().push(args.to_vec());
        Ok(Box::new(Exited))
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
    /// The encoder probes; `node --version` and yt-dlp never run, since neither is installed.
    runner: Arc<FakeRunner>,
    spawner: Arc<NoSpawner>,
    vars: EnvVars,
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
        Rig { _dir: dir, root, port, sup, http, runner, spawner: Arc::new(NoSpawner::default()), vars: EnvVars::default() }
    }

    fn layout(&self) -> Layout {
        Layout::new(&self.root)
    }

    fn wiring(&self) -> Wiring {
        let runner = self.runner.clone();
        Wiring {
            root: Root { dir: self.root.clone(), from_exe: false },
            vars: self.vars.clone(),
            client: self.sup.clone(),
            http: self.http.clone(),
            updater_http: Arc::new(FakeHttp::new()),
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
            stop_signal: Box::pin(std::future::pending()),
        }
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
        for _ in 0..60 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting until {what}");
    }
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
    let message = format!("Port {} is in use. Is another StreamLoop running? Stop it and this one starts by itself.", rig.port);
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
    app::remove_3_0_leftovers(&layout);

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
