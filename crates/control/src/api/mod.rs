//! The app's REST API on 127.0.0.1:7654: what Express did around the routes in `src/server/index.ts`
//! (JSON body parsing, the API token check and Express's own 404 page) and the
//! routes of `api.ts`, one file per group.

mod autostart;
mod cache;
mod config;
mod logs;
mod misc;
mod player;
mod playlist;
mod setup;
mod status;
mod stream_key;
mod update;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Body;
use axum::extract::{OriginalUri, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;
use serde_json::{json, Value};
use streamloop_common::config::Config;
use streamloop_common::local_only;
use streamloop_common::paths::Layout;
use tokio::sync::watch;

use crate::autostart_store::AutostartStore;
use crate::discord::Notifier;
use crate::launcher::{Launcher, LauncherState};
use crate::playlists::PlaylistCache;
use crate::runner::Runner;
use crate::setup_check::SetupReport;
use crate::supervisor_client::SupervisorApi;
use crate::timeline::EventLog;
use crate::twitch::TwitchStatus;

pub use autostart::startup_folder;
pub(crate) use autostart::{decode_vbs, vbs_target};
pub use setup::{free_mb, setup_check_with};
pub use status::{stream_state, StreamState};
pub use update::{ExeUpdates, Updates};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
/// Runs the setup check for an install and the playlist's video lengths in seconds.
pub type RunSetupCheck = Arc<dyn Fn(Layout, Vec<f64>) -> BoxFuture<Result<SetupReport, String>> + Send + Sync>;
/// Turns a plain stream key into the blob stored in `streamKeyProtected`.
pub type Protect = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;
pub type ApplyConfig = Arc<dyn Fn(&Config) + Send + Sync>;
/// Returns at once; whatever it starts (the exit, the shutdown) runs on its own.
pub type Trigger = Arc<dyn Fn() + Send + Sync>;
/// A config save rebuilds Discord and the Twitch checks, so routes ask for the current ones.
pub type GetNotifier = Arc<dyn Fn() -> Arc<dyn Notifier> + Send + Sync>;
pub type GetTwitchStatus = Arc<dyn Fn() -> TwitchStatus + Send + Sync>;

/// What the routes ask of the launcher.
pub trait LauncherControl: Send + Sync {
    fn state(&self) -> LauncherState;
    fn is_stopped_by_user(&self) -> bool;
    fn mark_stopped_by_user(&self);
    fn launch_now(&self);
    fn wait_reason(&self) -> Option<String>;
    fn block_reason(&self) -> Option<String>;
}

impl LauncherControl for Launcher {
    fn state(&self) -> LauncherState {
        Launcher::state(self)
    }

    fn is_stopped_by_user(&self) -> bool {
        Launcher::is_stopped_by_user(self)
    }

    fn mark_stopped_by_user(&self) {
        Launcher::mark_stopped_by_user(self)
    }

    fn launch_now(&self) {
        Launcher::launch_now(self)
    }

    fn wait_reason(&self) -> Option<String> {
        Launcher::wait_reason(self)
    }

    fn block_reason(&self) -> Option<String> {
        Launcher::block_reason(self)
    }
}

/// Everything the routes use, as the control process wires it.
pub struct AppDeps {
    /// The config the routes read; a save replaces it before `apply_config` runs.
    pub config: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub layout: Layout,
    pub client: Arc<dyn SupervisorApi>,
    pub launcher: Arc<dyn LauncherControl>,
    pub timeline: Arc<EventLog>,
    pub playlists: Arc<PlaylistCache>,
    pub updates: Arc<dyn Updates>,
    pub notifier: GetNotifier,
    pub twitch: GetTwitchStatus,
    pub uptime_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// The Windows Startup folder that may hold the `StreamLoop.vbs` 3.1 wrote; tests point it at a temp folder.
    pub startup_dir: PathBuf,
    /// The Run value that starts the app at logon.
    pub autostart: Arc<dyn AutostartStore>,
    /// `STREAMLOOP_TEST_HOOKS=1` only: adds `POST /test/exit-75`.
    pub test_hooks: bool,
    /// Runs `yt-dlp -U`.
    pub runner: Arc<dyn Runner>,
    pub setup_check: RunSetupCheck,
    /// `streamloop_common::secret::protect`, so the key never leaves this process.
    pub protect: Protect,
    /// Takes a just-saved config into the running process (Discord, Twitch, launcher).
    pub apply_config: ApplyConfig,
    /// Exits 75 once the control process's own timers are stopped.
    pub trigger_restart: Trigger,
    /// Exits 0; the supervisor and the stream keep running.
    pub trigger_shutdown: Trigger,
    /// Stops the supervisor, then exits 0.
    pub trigger_quit: Trigger,
}

/// Shared by every handler.
pub struct AppState {
    /// 32 random bytes as hex, new per process start. Every POST must send it as `x-api-token`.
    pub token: String,
    pub config: Arc<RwLock<Config>>,
    pub config_path: PathBuf,
    pub layout: Layout,
    pub client: Arc<dyn SupervisorApi>,
    pub launcher: Arc<dyn LauncherControl>,
    pub timeline: Arc<EventLog>,
    pub playlists: Arc<PlaylistCache>,
    pub updates: Arc<dyn Updates>,
    pub notifier: GetNotifier,
    pub twitch: GetTwitchStatus,
    pub uptime_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub startup_dir: PathBuf,
    pub autostart: Arc<dyn AutostartStore>,
    pub test_hooks: bool,
    pub runner: Arc<dyn Runner>,
    pub setup_check: RunSetupCheck,
    pub protect: Protect,
    pub apply_config: ApplyConfig,
    pub trigger_restart: Trigger,
    pub trigger_shutdown: Trigger,
    pub trigger_quit: Trigger,
    /// The setup check that runs now, so a second click waits for it instead of probing again.
    setup_run: Mutex<Option<watch::Receiver<Option<Result<SetupReport, String>>>>>,
}

impl AppState {
    pub fn new(deps: AppDeps) -> Self {
        let AppDeps {
            config,
            config_path,
            layout,
            client,
            launcher,
            timeline,
            playlists,
            updates,
            notifier,
            twitch,
            uptime_ms,
            startup_dir,
            autostart,
            test_hooks,
            runner,
            setup_check,
            protect,
            apply_config,
            trigger_restart,
            trigger_shutdown,
            trigger_quit,
        } = deps;
        Self {
            token: new_token(),
            config,
            config_path,
            layout,
            client,
            launcher,
            timeline,
            playlists,
            updates,
            notifier,
            twitch,
            uptime_ms,
            startup_dir,
            autostart,
            test_hooks,
            runner,
            setup_check,
            protect,
            apply_config,
            trigger_restart,
            trigger_shutdown,
            trigger_quit,
            setup_run: Mutex::new(None),
        }
    }

    pub(crate) fn config(&self) -> Config {
        self.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The parsed request body, `{}` unless the request carried JSON. Handlers take it as `Extension<JsonBody>`.
#[derive(Clone)]
pub struct JsonBody(pub Value);

/// `res.json()`: compact JSON as `application/json; charset=utf-8`.
pub fn json_reply(status: StatusCode, body: &Value) -> Response {
    text_reply(status, body.to_string())
}

/// `res.json()` of a typed value, in its own field order.
pub(crate) fn typed_reply<T: Serialize + ?Sized>(status: StatusCode, body: &T) -> Response {
    text_reply(status, serde_json::to_string(body).expect("a reply serialises"))
}

/// `body` is JSON text already.
pub(crate) fn text_reply(status: StatusCode, body: String) -> Response {
    let mut res = (status, body).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    res
}

pub(crate) fn error_reply(status: StatusCode, message: &str) -> Response {
    json_reply(status, &json!({ "error": message }))
}

/// What JavaScript's `trim()` and `\s` count as white space: Unicode's, except U+0085, plus the byte order mark.
pub(crate) fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// JavaScript truthiness, for the request fields `api.ts` tests with `if (x)`.
pub(crate) fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

// express.json's default limit.
const BODY_LIMIT: usize = 100 * 1024;

/// The whole API: every route of `api.ts` on the router base. `port` is the one the server is bound to.
pub fn router(state: Arc<AppState>, port: u16) -> Router {
    let mut routes = Router::new()
        .route("/status", get(status::status))
        .route("/state", get(status::now_playing))
        .route("/events", get(status::events))
        .route("/cache", get(cache::report))
        .route("/config", get(config::read).post(config::save))
        .route("/stream-key", post(stream_key::set))
        .route("/autostart", get(autostart::read).post(autostart::set))
        .route("/discord/test", post(misc::discord_test))
        .route("/discord/defaults", get(misc::discord_defaults))
        .route("/update/status", get(update::status))
        .route("/update/check", post(update::check))
        .route("/update/apply", post(update::apply))
        .route("/playlist/videos", get(playlist::videos))
        .route("/playlist/switch", post(playlist::switch))
        .route("/player/next", post(player::next))
        .route("/player/prev", post(player::prev))
        .route("/player/restart", post(player::restart))
        .route("/player/jump", post(player::jump))
        .route("/stream/stop", post(player::stop))
        .route("/stream/start", post(player::start))
        .route("/setup-check", get(setup::report))
        .route("/setup-check/run", post(setup::run))
        .route("/logs", get(logs::list))
        .route("/logs/{source}/{name}", get(logs::read))
        .route("/yt-dlp/update", post(misc::update_ytdlp))
        .route("/shutdown", post(misc::shutdown))
        .route("/quit", post(misc::quit));
    if state.test_hooks {
        routes = routes.route("/test/exit-75", post(misc::exit_75));
    }
    router_with(state, routes, port)
}

/// The base router with `api_routes` mounted under `/api`, behind the body parser and the token check,
/// and behind the browser check, which runs first on every path and method.
pub fn router_with(state: Arc<AppState>, api_routes: Router<Arc<AppState>>, port: u16) -> Router {
    let api = Router::new()
        .route("/token", get(token))
        .merge(api_routes)
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), api_guard));
    Router::new()
        .nest("/api", api)
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .with_state(state)
        .layer(middleware::from_fn_with_state(port, refuse_browsers))
}

/// Refuses a foreign Host or any Origin with 403 before routing, so not even `/api/token` answers a web page.
async fn refuse_browsers(State(port): State<u16>, req: Request, next: Next) -> Response {
    // A header that is not text still counts as present, and as a host nobody serves.
    let refused = {
        let text = |name: header::HeaderName| req.headers().get(name).map(|v| v.to_str().unwrap_or("?"));
        local_only::refused(text(header::HOST), text(header::ORIGIN), port)
    };
    if refused {
        return json_reply(StatusCode::FORBIDDEN, &json!({ "error": local_only::REFUSED }));
    }
    next.run(req).await
}

fn is_json(req: &Request) -> bool {
    req.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
}

/// What body-parser's strict JSON does with `raw`: nothing for an empty body, an object or array otherwise.
fn parse_strict(raw: &[u8]) -> Option<Value> {
    if raw.is_empty() {
        return Some(json!({}));
    }
    let first = raw.iter().find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))?;
    if *first != b'{' && *first != b'[' {
        return None;
    }
    serde_json::from_slice(raw).ok()
}

// The body, the parse error and the error text never reach the reply or the log: they may hold the stream key.
fn failure(kind: &str, status: StatusCode, message: &str) -> Response {
    tracing::warn!(kind = kind, status = status.as_u16(), "API request failed");
    json_reply(status, &json!({ "error": message }))
}

async fn api_guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let mut req = Request::from_parts(parts, Body::empty());
    let parsed = if is_json(&req) {
        let raw = match axum::body::to_bytes(body, BODY_LIMIT).await {
            Ok(b) => b,
            Err(_) => return failure("entity.too.large", StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
        };
        match parse_strict(&raw) {
            Some(v) => v,
            None => return failure("entity.parse.failed", StatusCode::BAD_REQUEST, "Invalid JSON"),
        }
    } else {
        json!({})
    };
    if req.method() == Method::POST {
        let sent = req.headers().get("x-api-token").and_then(|v| v.to_str().ok());
        if sent != Some(state.token.as_str()) {
            return json_reply(StatusCode::FORBIDDEN, &json!({ "error": "Forbidden: invalid API token" }));
        }
    }
    req.extensions_mut().insert(JsonBody(parsed));
    next.run(req).await
}

async fn token(State(state): State<Arc<AppState>>) -> Response {
    json_reply(StatusCode::OK, &json!({ "token": state.token }))
}

// Express's final handler, with the headers it sends.
fn express_page(status: StatusCode, content_type: &'static str, title: &str, text: &str) -> Response {
    let body = format!("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n</head>\n<body>\n<pre>{text}</pre>\n</body>\n</html>\n");
    let mut res = (status, body).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    res
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

async fn not_found(method: Method, OriginalUri(uri): OriginalUri) -> Response {
    let text = format!("Cannot {method} {}", escape_html(uri.path()));
    express_page(StatusCode::NOT_FOUND, "text/html; charset=utf-8", "Error", &text)
}

/// Stand-ins for the launcher and the updater, for tests of the routes.
pub mod fake {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use tokio::sync::Notify;

    use super::{LauncherControl, Updates};
    use crate::launcher::LauncherState;
    use crate::updater::UpdateInfo;

    /// Starts `running`. `mark_stopped_by_user` and `launch_now` flip the stop flag as the real launcher does.
    pub struct FakeLauncher {
        state: Mutex<LauncherState>,
        stopped: AtomicBool,
        wait: Mutex<Option<String>>,
        block: Mutex<Option<String>>,
        marks: AtomicUsize,
        launches: AtomicUsize,
    }

    impl Default for FakeLauncher {
        fn default() -> Self {
            Self {
                state: Mutex::new(LauncherState::Running),
                stopped: AtomicBool::new(false),
                wait: Mutex::new(None),
                block: Mutex::new(None),
                marks: AtomicUsize::new(0),
                launches: AtomicUsize::new(0),
            }
        }
    }

    impl FakeLauncher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn set_state(&self, s: LauncherState) {
            *self.state.lock().unwrap() = s;
        }

        pub fn set_stopped(&self, stopped: bool) {
            self.stopped.store(stopped, Ordering::SeqCst);
        }

        pub fn set_wait_reason(&self, r: Option<&str>) {
            *self.wait.lock().unwrap() = r.map(String::from);
        }

        pub fn set_block_reason(&self, r: Option<&str>) {
            *self.block.lock().unwrap() = r.map(String::from);
        }

        pub fn marks(&self) -> usize {
            self.marks.load(Ordering::SeqCst)
        }

        pub fn launches(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }
    }

    impl LauncherControl for FakeLauncher {
        fn state(&self) -> LauncherState {
            *self.state.lock().unwrap()
        }

        fn is_stopped_by_user(&self) -> bool {
            self.stopped.load(Ordering::SeqCst)
        }

        fn mark_stopped_by_user(&self) {
            self.marks.fetch_add(1, Ordering::SeqCst);
            self.stopped.store(true, Ordering::SeqCst);
        }

        fn launch_now(&self) {
            self.launches.fetch_add(1, Ordering::SeqCst);
            self.stopped.store(false, Ordering::SeqCst);
        }

        fn wait_reason(&self) -> Option<String> {
            self.wait.lock().unwrap().clone()
        }

        fn block_reason(&self) -> Option<String> {
            self.block.lock().unwrap().clone()
        }
    }

    /// `land` answers `land_result`; after `hold` it waits until `release` first.
    pub struct FakeUpdates {
        status: Mutex<UpdateInfo>,
        landing: AtomicBool,
        confirming: AtomicBool,
        land_result: Mutex<Result<(), String>>,
        checks: AtomicUsize,
        lands: AtomicUsize,
        landed: AtomicUsize,
        held: Mutex<bool>,
        released: Notify,
    }

    impl FakeUpdates {
        pub fn new(status: UpdateInfo) -> Self {
            Self {
                status: Mutex::new(status),
                landing: AtomicBool::new(false),
                confirming: AtomicBool::new(false),
                land_result: Mutex::new(Ok(())),
                checks: AtomicUsize::new(0),
                lands: AtomicUsize::new(0),
                landed: AtomicUsize::new(0),
                held: Mutex::new(false),
                released: Notify::new(),
            }
        }

        pub fn set_status(&self, s: UpdateInfo) {
            *self.status.lock().unwrap() = s;
        }

        pub fn set_landing(&self, on: bool) {
            self.landing.store(on, Ordering::SeqCst);
        }

        /// A confirmation counts as a landing in progress too, as the control process reports it.
        pub fn set_confirming(&self, on: bool) {
            self.confirming.store(on, Ordering::SeqCst);
        }

        pub fn set_land_result(&self, r: Result<(), String>) {
            *self.land_result.lock().unwrap() = r;
        }

        pub fn hold(&self) {
            *self.held.lock().unwrap() = true;
        }

        pub fn release(&self) {
            *self.held.lock().unwrap() = false;
            self.released.notify_waiters();
        }

        pub fn checks(&self) -> usize {
            self.checks.load(Ordering::SeqCst)
        }

        /// Calls to `land` so far, finished or not.
        pub fn lands(&self) -> usize {
            self.lands.load(Ordering::SeqCst)
        }

        /// Calls to `land` that have returned.
        pub fn landed(&self) -> usize {
            self.landed.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Updates for FakeUpdates {
        fn status(&self) -> UpdateInfo {
            self.status.lock().unwrap().clone()
        }

        async fn check(&self) -> UpdateInfo {
            self.checks.fetch_add(1, Ordering::SeqCst);
            self.status()
        }

        fn landing_in_progress(&self) -> bool {
            self.landing.load(Ordering::SeqCst) || self.confirming.load(Ordering::SeqCst)
        }

        fn confirming(&self) -> bool {
            self.confirming.load(Ordering::SeqCst)
        }

        fn begin_quit(&self) -> bool {
            !self.landing_in_progress()
        }

        async fn land(&self) -> Result<(), String> {
            self.lands.fetch_add(1, Ordering::SeqCst);
            loop {
                let waiting = self.released.notified();
                tokio::pin!(waiting);
                waiting.as_mut().enable();
                if !*self.held.lock().unwrap() {
                    break;
                }
                waiting.await;
            }
            self.landed.fetch_add(1, Ordering::SeqCst);
            self.land_result.lock().unwrap().clone()
        }
    }
}
