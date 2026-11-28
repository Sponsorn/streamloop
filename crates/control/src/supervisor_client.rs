//! HTTP client for the supervisor on 127.0.0.1:4417. It never fails: callers run on timers and
//! get a result value, whatever the supervisor or the network does.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use streamloop_common::api::{CacheReport, ControlCmd, ControlRequest, EncoderStatus, ReloadRequest, SupervisorStatus};

use crate::http::{Http, HttpError, HttpErrorKind, HttpRequest};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:4417";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlResult {
    pub ok: bool,
    pub error: Option<String>,
    /// Set for a `restart-encoder` that found a restart already pending.
    pub already_restarting: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadResult {
    pub ok: bool,
    pub restart_required: bool,
    pub error: Option<String>,
    /// The socket code of a failed connection, `ECONNREFUSED` when nothing listens.
    pub code: Option<String>,
    /// No answer came: the connection failed or nothing answered within the timeout.
    pub unanswered: bool,
}

#[async_trait]
pub trait SupervisorApi: Send + Sync {
    /// `None` when the supervisor does not answer, answers non-200 or sends something that is not a status object.
    async fn status(&self) -> Option<SupervisorStatus>;
    /// `reason` is a note for `restart-encoder`, which the supervisor logs. `id` names a jump's video and wins over `index` when its list has it.
    async fn control(&self, cmd: ControlCmd, index: Option<u32>, reason: Option<String>, id: Option<String>) -> ControlResult;
    async fn reload(&self, req: ReloadRequest) -> ReloadResult;
    async fn shutdown(&self) -> bool;
    /// `GET /cache`. `None` when the supervisor does not answer, answers non-200 (a 3.1.x one has no such
    /// route) or sends something that is not a report.
    async fn cache(&self) -> Option<CacheReport> {
        None
    }
}

pub struct SupervisorClient {
    http: Arc<dyn Http>,
    base_url: String,
    timeout: Duration,
}

struct Answer {
    status: u16,
    json: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Answer {
    fn error_text(&self) -> String {
        match self.json.as_ref().and_then(|j| j.get("error")) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Null) | None => format!("HTTP {}", self.status),
            Some(other) => other.to_string(),
        }
    }
}

/// Node's fetch reports a failed connection as "fetch failed" and an expired timeout as an abort.
fn error_text(e: &HttpError) -> String {
    match e.kind {
        HttpErrorKind::Timeout => "The operation was aborted due to timeout".into(),
        _ => "fetch failed".into(),
    }
}

impl SupervisorClient {
    pub fn new(http: Arc<dyn Http>, base_url: Option<&str>, timeout: Option<Duration>) -> Self {
        Self { http, base_url: base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/').to_string(), timeout: timeout.unwrap_or(DEFAULT_TIMEOUT) }
    }

    /// `body` is the exact bytes to send, so key order is the caller's serialisation.
    async fn call(&self, path: &str, body: Option<Vec<u8>>) -> Result<Answer, HttpError> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = HttpRequest::get(&url);
        if let Some(body) = body {
            req.method = "POST".into();
            req.headers = vec![("content-type".into(), "application/json".into())];
            req.body = Some(body);
        }
        req.timeout = Some(self.timeout);
        let res = match tokio::time::timeout(self.timeout, self.http.send(req)).await {
            Ok(res) => res?,
            Err(_) => return Err(HttpError::timeout("timed out")),
        };
        let json = match serde_json::from_slice::<serde_json::Value>(&res.body) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            _ => None,
        };
        Ok(Answer { status: res.status, json })
    }
}

#[async_trait]
impl SupervisorApi for SupervisorClient {
    async fn status(&self) -> Option<SupervisorStatus> {
        let answer = self.call("/status", None).await.ok()?;
        if answer.status != 200 {
            return None;
        }
        let json = Value::Object(answer.json?);
        // A supervisor whose status changed shape still answers: its version and encoder are what the version
        // rule needs to replace it, instead of finding the port taken on every relaunch.
        Some(SupervisorStatus::deserialize(&json).unwrap_or_else(|_| version_and_encoder(&json)))
    }

    async fn control(&self, cmd: ControlCmd, index: Option<u32>, reason: Option<String>, id: Option<String>) -> ControlResult {
        let req = ControlRequest { cmd, index: index.map(i64::from), reason, id };
        let body = serde_json::to_vec(&req).expect("a control request serialises");
        match self.call("/control", Some(body)).await {
            Err(e) => ControlResult { ok: false, error: Some(error_text(&e)), already_restarting: false },
            Ok(a) if a.status != 200 => ControlResult { ok: false, error: Some(a.error_text()), already_restarting: false },
            Ok(a) => {
                let pending = a.json.as_ref().and_then(|j| j.get("alreadyRestarting")).is_some_and(is_truthy);
                ControlResult { ok: true, error: None, already_restarting: pending }
            }
        }
    }

    async fn reload(&self, req: ReloadRequest) -> ReloadResult {
        let body = serde_json::to_vec(&req).expect("a reload request serialises");
        match self.call("/reload", Some(body)).await {
            Err(e) => {
                let code = match &e.kind {
                    HttpErrorKind::Connect { code } => Some(code.clone()),
                    _ => None,
                };
                ReloadResult { ok: false, restart_required: false, error: Some(error_text(&e)), code, unanswered: true }
            }
            Ok(a) if a.status != 200 => ReloadResult { ok: false, restart_required: false, error: Some(a.error_text()), code: None, unanswered: false },
            Ok(a) => {
                let restart_required = a.json.as_ref().and_then(|j| j.get("restartRequired")) == Some(&serde_json::Value::Bool(true));
                ReloadResult { ok: true, restart_required, error: None, code: None, unanswered: false }
            }
        }
    }

    async fn shutdown(&self) -> bool {
        matches!(self.call("/shutdown", Some(b"{}".to_vec())).await, Ok(a) if a.status == 200)
    }

    async fn cache(&self) -> Option<CacheReport> {
        let answer = self.call("/cache", None).await.ok()?;
        if answer.status != 200 {
            return None;
        }
        CacheReport::deserialize(Value::Object(answer.json?)).ok()
    }
}

/// `version` and `encoder.alive`, each only when it has the type this side expects.
fn version_and_encoder(json: &Value) -> SupervisorStatus {
    let alive = json.get("encoder").and_then(|e| e.get("alive")).and_then(Value::as_bool);
    SupervisorStatus {
        version: json.get("version").and_then(Value::as_str).map(str::to_string),
        encoder: alive.map(|alive| EncoderStatus { alive: Some(alive), ..EncoderStatus::default() }),
        ..SupervisorStatus::default()
    }
}

/// JavaScript truthiness for the one JSON field Node tests with `?`.
fn is_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// A stand-in supervisor for tests of the code that calls one.
pub mod fake {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use streamloop_common::api::{CacheReport, ControlCmd, ReloadRequest, SupervisorStatus};
    use tokio::sync::Notify;

    use super::{ControlResult, ReloadResult, SupervisorApi};

    #[derive(Debug, Clone, PartialEq)]
    pub enum Call {
        Status,
        Control { cmd: ControlCmd, index: Option<u32>, reason: Option<String>, id: Option<String> },
        Reload(ReloadRequest),
        Shutdown,
        Cache,
    }

    pub struct FakeSupervisor {
        status: Mutex<Option<SupervisorStatus>>,
        cache: Mutex<Option<CacheReport>>,
        control_result: Mutex<ControlResult>,
        reload_result: Mutex<ReloadResult>,
        shutdown_result: Mutex<bool>,
        stops_on_shutdown: Mutex<bool>,
        calls: Mutex<Vec<Call>>,
        hanging: Mutex<bool>,
        released: Notify,
    }

    impl Default for FakeSupervisor {
        fn default() -> Self {
            Self {
                status: Mutex::new(None),
                cache: Mutex::new(None),
                control_result: Mutex::new(ControlResult { ok: true, ..ControlResult::default() }),
                reload_result: Mutex::new(ReloadResult { ok: true, ..ReloadResult::default() }),
                shutdown_result: Mutex::new(true),
                stops_on_shutdown: Mutex::new(false),
                calls: Mutex::new(Vec::new()),
                hanging: Mutex::new(false),
                released: Notify::new(),
            }
        }
    }

    impl FakeSupervisor {
        pub fn new() -> Self {
            Self::default()
        }

        /// `None` makes `status()` answer as an unreachable supervisor does.
        pub fn set_status(&self, status: Option<SupervisorStatus>) {
            *self.status.lock().unwrap() = status;
        }

        /// `None` makes `cache()` answer as a supervisor without the route does.
        pub fn set_cache(&self, c: Option<CacheReport>) {
            *self.cache.lock().unwrap() = c;
        }

        pub fn set_control_result(&self, r: ControlResult) {
            *self.control_result.lock().unwrap() = r;
        }

        pub fn set_reload_result(&self, r: ReloadResult) {
            *self.reload_result.lock().unwrap() = r;
        }

        pub fn set_shutdown_result(&self, ok: bool) {
            *self.shutdown_result.lock().unwrap() = ok;
        }

        /// From now on `shutdown` also makes `status` answer as an unreachable supervisor does.
        pub fn stop_answering_on_shutdown(&self) {
            *self.stops_on_shutdown.lock().unwrap() = true;
        }

        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        /// From now on every call is recorded and then never answers until `release`.
        pub fn hang(&self) {
            *self.hanging.lock().unwrap() = true;
        }

        pub fn release(&self) {
            *self.hanging.lock().unwrap() = false;
            self.released.notify_waiters();
        }

        async fn enter(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
            loop {
                let waiting = self.released.notified();
                tokio::pin!(waiting);
                waiting.as_mut().enable();
                if !*self.hanging.lock().unwrap() {
                    return;
                }
                waiting.await;
            }
        }
    }

    #[async_trait]
    impl SupervisorApi for FakeSupervisor {
        async fn status(&self) -> Option<SupervisorStatus> {
            self.enter(Call::Status).await;
            self.status.lock().unwrap().clone()
        }

        async fn control(&self, cmd: ControlCmd, index: Option<u32>, reason: Option<String>, id: Option<String>) -> ControlResult {
            self.enter(Call::Control { cmd, index, reason, id }).await;
            self.control_result.lock().unwrap().clone()
        }

        async fn reload(&self, req: ReloadRequest) -> ReloadResult {
            self.enter(Call::Reload(req)).await;
            self.reload_result.lock().unwrap().clone()
        }

        async fn shutdown(&self) -> bool {
            self.enter(Call::Shutdown).await;
            if *self.stops_on_shutdown.lock().unwrap() {
                self.set_status(None);
            }
            *self.shutdown_result.lock().unwrap()
        }

        async fn cache(&self) -> Option<CacheReport> {
            self.enter(Call::Cache).await;
            self.cache.lock().unwrap().clone()
        }
    }
}
