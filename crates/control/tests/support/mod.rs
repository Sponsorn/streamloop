//! An `AppState` built from fakes, for the API tests.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use streamloop_common::config::Config;
use streamloop_common::encoder::Encoder;
use streamloop_common::paths::Layout;
use streamloop_control::api::fake::{FakeLauncher, FakeUpdates};
use streamloop_control::api::{self, AppDeps, AppState};
use streamloop_control::clock::{Clock, SystemClock};
use streamloop_control::discord::{EmbedField, Level, Notifier};
use streamloop_control::events::Alert;
use streamloop_control::playlists::PlaylistCache;
use streamloop_control::readback::ReadbackStatus;
use streamloop_control::runner::FakeRunner;
use streamloop_control::setup_check::{ReportCache, ReportEncoder, SetupReport};
use streamloop_control::supervisor_client::fake::FakeSupervisor;
use streamloop_control::timeline::{EventLog, EventStore};
use streamloop_control::twitch::TwitchStatus;
use streamloop_control::updater::{UpdateInfo, UpdateStatus};
use tempfile::TempDir;
use tower::ServiceExt;

pub const TOKEN: &str = "test-token";
pub const KEY: &str = "live_TESTKEY";
pub const BLOB: &str = "AQAAANCMnd8BFdERjHoAwE_fake_blob";

/// `makeConfig()` of `api.test.ts`.
pub fn config_json() -> Value {
    json!({
        "port": 7654,
        "playlists": [{ "id": "PLaaa", "name": "A" }, { "id": "PLbbb", "name": "B" }, { "id": "PLccc", "name": "C" }],
        "discord": { "webhookUrl": "https://discord.example/hook", "botName": "", "avatarUrl": "", "rolePing": "" },
        "supervisor": {
            "ingestUrl": "rtmp://live.twitch.tv/app", "streamKeyProtected": BLOB, "encoder": "h264_nvenc", "cacheBudgetMb": 300,
            "readAheadSeconds": 20, "playerClient": "mweb,web_embedded", "overlay": true, "freezeSeconds": 20, "silenceSeconds": 30
        },
        "autoUpdateCheck": true, "updateCheckIntervalMs": 21600000, "autoUpdateYtdlp": true, "autoOpenAdmin": true, "autoStartStream": true,
        "twitchClientId": "id", "twitchClientSecret": "secret", "twitchChannel": "chan", "twitchLivenessEnabled": false,
        "twitchPollIntervalMs": 60000, "ytdlCookiesFromBrowser": ""
    })
}

/// `idleUpdate()` of `api.test.ts`, with R1's `kind` and no parts.
pub fn idle_update() -> UpdateInfo {
    UpdateInfo {
        current_version: "3.0.5".into(),
        latest_version: Some("3.0.6".into()),
        update_available: true,
        status: UpdateStatus::Idle,
        error: None,
        is_dev_mode: false,
        node_notice: None,
        kind: Some("app".into()),
        parts: vec![],
        same_version: false,
        download_percent: None,
    }
}

pub fn report() -> SetupReport {
    SetupReport {
        at: "now".into(),
        encoder: ReportEncoder { chosen: Encoder::Nvenc, tried: vec![Encoder::Nvenc], software: false, works: true },
        cache: ReportCache { free_mb: 1, recommended_mb: 50, warning: None },
        dependencies: vec![],
    }
}

#[derive(Default)]
pub struct RecordingNotifier(pub Mutex<Vec<(String, Level)>>);

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn notify_critical(&self, _message: &str) {}
    async fn notify_event(&self, _alert: Alert) {}
    async fn notify_twitch_mismatch(&self, _channel: &str) {}
    async fn notify_twitch_restart(&self, _channel: &str) {}
    async fn send(&self, content: &str, level: Level, _fields: Option<Vec<EmbedField>>) {
        self.0.lock().unwrap().push((content.to_string(), level));
    }
    async fn flush(&self) {}
}

#[derive(Clone, Default)]
pub struct Capture(pub Arc<Mutex<Vec<u8>>>);

impl Capture {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

/// One process-wide capture at TRACE, so every level of every test lands in it.
pub fn captured_logs() -> &'static Capture {
    static LOGS: OnceLock<Capture> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Capture::default();
        let subscriber = tracing_subscriber::fmt().with_writer(logs.clone()).with_ansi(false).with_max_level(tracing::Level::TRACE).finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other test installs a global subscriber");
        logs
    })
}

/// The fakes behind one `AppState`, kept so a test can set them up and look at them afterwards.
pub struct Rig {
    pub dir: TempDir,
    pub layout: Layout,
    pub config: Arc<RwLock<Config>>,
    pub sup: Arc<FakeSupervisor>,
    pub launcher: Arc<FakeLauncher>,
    pub updates: Arc<FakeUpdates>,
    /// yt-dlp as the playlist cache runs it.
    pub ytdlp: Arc<FakeRunner>,
    /// `yt-dlp -U` and `--version`.
    pub runner: Arc<FakeRunner>,
    pub timeline: Arc<EventLog>,
    pub notifier: Arc<RecordingNotifier>,
    pub setup_runs: Arc<Mutex<Vec<(PathBuf, Vec<f64>)>>>,
    pub protected: Arc<Mutex<Vec<String>>>,
    pub applied: Arc<AtomicUsize>,
    pub restarts: Arc<AtomicUsize>,
    pub shutdowns: Arc<AtomicUsize>,
}

impl Rig {
    pub fn new() -> Rig {
        captured_logs();
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(&dir.path().join("root"));
        std::fs::create_dir_all(&layout.app_dir).unwrap();
        std::fs::write(&layout.config, config_json().to_string()).unwrap();
        let config = streamloop_common::config::load(&layout.config).unwrap();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let ytdlp = Arc::new(FakeRunner::new());
        let timeline = Arc::new(EventLog::new(EventStore::new(&dir.path().join("timeline"), clock.clone())));
        std::fs::create_dir_all(dir.path().join("startup")).unwrap();
        Rig {
            layout,
            config: Arc::new(RwLock::new(config)),
            sup: Arc::new(FakeSupervisor::new()),
            launcher: Arc::new(FakeLauncher::new()),
            updates: Arc::new(FakeUpdates::new(idle_update())),
            ytdlp,
            runner: Arc::new(FakeRunner::new()),
            timeline,
            notifier: Arc::new(RecordingNotifier::default()),
            setup_runs: Arc::new(Mutex::new(Vec::new())),
            protected: Arc::new(Mutex::new(Vec::new())),
            applied: Arc::new(AtomicUsize::new(0)),
            restarts: Arc::new(AtomicUsize::new(0)),
            shutdowns: Arc::new(AtomicUsize::new(0)),
            dir,
        }
    }

    pub fn startup_dir(&self) -> PathBuf {
        self.dir.path().join("startup")
    }

    pub fn deps(&self) -> AppDeps {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let notifier: Arc<dyn Notifier> = self.notifier.clone();
        let (setup_runs, protected) = (self.setup_runs.clone(), self.protected.clone());
        let (applied, restarts, shutdowns) = (self.applied.clone(), self.restarts.clone(), self.shutdowns.clone());
        AppDeps {
            config: self.config.clone(),
            config_path: self.layout.config.clone(),
            layout: self.layout.clone(),
            client: self.sup.clone(),
            launcher: self.launcher.clone(),
            timeline: self.timeline.clone(),
            playlists: Arc::new(PlaylistCache::new(self.ytdlp.clone(), PathBuf::from("yt-dlp.exe"), String::new(), clock)),
            updates: self.updates.clone(),
            notifier: Arc::new(move || notifier.clone()),
            twitch: Arc::new(|| TwitchStatus {
                enabled: false,
                channel_live: None,
                encoder_alive: None,
                consecutive_mismatches: 0,
                mismatch_threshold: 3,
                last_check_at: None,
                last_restart_at: None,
                restart_count: 0,
                readback: ReadbackStatus::off(),
            }),
            uptime_ms: Arc::new(|| 1234),
            startup_dir: self.startup_dir(),
            test_hooks: false,
            runner: self.runner.clone(),
            setup_check: Arc::new(move |layout: Layout, durations: Vec<f64>| {
                setup_runs.lock().unwrap().push((layout.root.clone(), durations));
                Box::pin(async { Ok(report()) })
            }),
            protect: Arc::new(move |key: &str| {
                protected.lock().unwrap().push(key.to_string());
                Ok(BLOB.to_string())
            }),
            apply_config: Arc::new(move |_c: &Config| {
                applied.fetch_add(1, Ordering::SeqCst);
            }),
            trigger_restart: Arc::new(move || {
                restarts.fetch_add(1, Ordering::SeqCst);
            }),
            trigger_shutdown: Arc::new(move || {
                shutdowns.fetch_add(1, Ordering::SeqCst);
            }),
        }
    }

    pub fn app(&self) -> Router {
        self.app_with(|_| {})
    }

    pub fn app_with(&self, change: impl FnOnce(&mut AppDeps)) -> Router {
        let mut deps = self.deps();
        change(&mut deps);
        let mut state = AppState::new(deps);
        state.token = TOKEN.to_string();
        api::router(Arc::new(state))
    }

    pub fn config_text(&self) -> String {
        std::fs::read_to_string(&self.layout.config).unwrap()
    }

    pub fn saved(&self) -> Value {
        serde_json::from_str(&self.config_text()).unwrap()
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub content_type: String,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }

    pub fn error(&self) -> String {
        self.json()["error"].as_str().unwrap_or_else(|| panic!("no error text: {}", self.body)).to_string()
    }
}

pub async fn call(app: &Router, req: Request<Body>) -> Reply {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    let body = String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    Reply { status, content_type, body }
}

pub async fn get(app: &Router, path: &str) -> Reply {
    call(app, Request::builder().uri(format!("/api{path}")).body(Body::empty()).unwrap()).await
}

pub async fn post(app: &Router, path: &str, body: Value) -> Reply {
    post_raw(app, path, &body.to_string()).await
}

pub async fn post_raw(app: &Router, path: &str, body: &str) -> Reply {
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api{path}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-token", TOKEN)
        .body(Body::from(body.to_string()))
        .unwrap();
    call(app, req).await
}

pub async fn post_without_token(app: &Router, path: &str, body: Value) -> Reply {
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api{path}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    call(app, req).await
}

/// Waits up to 3 s for `done`.
pub async fn eventually(what: &str, done: impl Fn() -> bool) {
    for _ in 0..600 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}
