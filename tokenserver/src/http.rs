// The HTTP face of bgutil-ytdlp-pot-provider 2.0.0's token server.
// Copyright (c) Brainicism and contributors, GPL-3.0.

use std::io::Read;
use std::net::SocketAddr;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::session::{PotAnswer, PotRequest, Sessions};

/// The protocol version the 2.0.0 plugin checks, not this server's own.
pub const PROTOCOL_VERSION: &str = "2.0.0";

const MAX_BODY: usize = 64 * 1024;
/// Threads that read requests and answer everything but a mint.
const FRONT_THREADS: usize = 8;
/// Threads that run `get_pot`. A mint can block for over a minute; it never holds a front thread.
pub const MINT_WORKERS: usize = 4;
/// Accepted mints waiting for a worker; a request past this gets 503.
pub const MINT_QUEUE: usize = 16;

pub trait Service: Send + Sync {
    fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String>;
    fn invalidate_caches(&self);
    fn invalidate_it(&self);
    fn minter_keys(&self) -> Vec<String>;
}

impl Service for Sessions {
    fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        Sessions::get_pot(self, r)
    }
    fn invalidate_caches(&self) {
        Sessions::invalidate_caches(self);
    }
    fn invalidate_it(&self) {
        Sessions::invalidate_it(self);
    }
    fn minter_keys(&self) -> Vec<String> {
        Sessions::minter_keys(self)
    }
}

/// What a request needs next: an answer now, or a mint on a worker.
pub enum Routed {
    Reply(u16, String),
    Mint(PotRequest),
}

fn error(status: u16, message: &str) -> Routed {
    Routed::Reply(status, json!({ "error": message }).to_string())
}

/// JavaScript truthiness, which is what 2.0.0's checks on these fields use.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => true,
    }
}

fn present(v: Option<&Value>) -> bool {
    !matches!(v, None | Some(Value::Null))
}

pub fn route(
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: &[u8],
    svc: &dyn Service,
    uptime_s: f64,
) -> Routed {
    let path = path.split('?').next().unwrap_or("");
    match (method, path) {
        ("GET", "/ping") => Routed::Reply(
            200,
            json!({
                "server_uptime": uptime_s,
                "version": PROTOCOL_VERSION,
                "implementation": format!("streamloop-pot {}", env!("CARGO_PKG_VERSION")),
            })
            .to_string(),
        ),
        ("GET", "/minter_cache") => Routed::Reply(200, json!(svc.minter_keys()).to_string()),
        ("POST", "/invalidate_caches") => {
            svc.invalidate_caches();
            Routed::Reply(204, String::new())
        }
        ("POST", "/invalidate_it") => {
            svc.invalidate_it();
            Routed::Reply(204, String::new())
        }
        ("POST", "/get_pot") => route_get_pot(content_type, body),
        _ => error(404, "Not found"),
    }
}

fn route_get_pot(content_type: Option<&str>, body: &[u8]) -> Routed {
    let essence = content_type
        .and_then(|c| c.split(';').next())
        .map(|c| c.trim().to_ascii_lowercase());
    if essence.as_deref() != Some("application/json") {
        return error(415, "Content-Type must be application/json");
    }
    if body.len() > MAX_BODY {
        return error(413, "Request body too large");
    }
    let Ok(Value::Object(b)) = serde_json::from_slice::<Value>(body) else {
        return error(400, "Request body must be a JSON object");
    };
    if truthy(b.get("data_sync_id")) {
        return error(400, "data_sync_id is deprecated, use content_binding instead");
    }
    if truthy(b.get("visitor_data")) {
        return error(400, "visitor_data is deprecated, use content_binding instead");
    }
    if truthy(b.get("disable_innertube")) {
        return error(
            400,
            "disable_innertube is deprecated because the /Create endpoint doesn't work anymore",
        );
    }
    if present(b.get("proxy")) || present(b.get("source_address")) || truthy(b.get("disable_tls_verification")) {
        return error(400, "proxy, source_address and disable_tls_verification are not supported");
    }
    let content_binding = match b.get("content_binding") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let innertube_context = b.get("innertube_context").filter(|v| !v.is_null()).cloned();
    let has_visitor = innertube_context
        .as_ref()
        .and_then(|c| c.pointer("/client/visitorData"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if content_binding.is_empty() && !has_visitor {
        return error(400, "content_binding is required");
    }
    Routed::Mint(PotRequest {
        content_binding,
        challenge: b.get("challenge").filter(|v| !v.is_null()).cloned(),
        bypass_cache: truthy(b.get("bypass_cache")),
        innertube_context,
    })
}

pub fn mint_reply(result: Result<PotAnswer, String>) -> (u16, String) {
    match result {
        Ok(a) => (
            200,
            json!({
                "contentBinding": a.content_binding,
                "poToken": a.po_token,
                "expiresAt": iso8601(a.expires_at),
            })
            .to_string(),
        ),
        Err(e) => (500, json!({ "error": e }).to_string()),
    }
}

/// Route and, for a mint, run it on this thread.
pub fn handle(
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: &[u8],
    svc: &dyn Service,
    uptime_s: f64,
) -> (u16, String) {
    match route(method, path, content_type, body, svc, uptime_s) {
        Routed::Reply(s, b) => (s, b),
        Routed::Mint(r) => mint_reply(svc.get_pot(&r)),
    }
}

/// `2026-11-08T12:00:00.000Z`, as JavaScript's `toISOString` writes it.
fn iso8601(t: SystemTime) -> String {
    let ms = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let (secs, millis) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, tod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    )
}

type Mint = (Request, PotRequest);

pub struct Running {
    server: Arc<Server>,
    threads: Vec<JoinHandle<()>>,
    addr: SocketAddr,
}

impl Running {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        for _ in 0..FRONT_THREADS {
            self.server.unblock();
        }
        // The workers end when the front threads drop the queue's sender; one stuck in a
        // mint is left to finish on its own.
        for t in self.threads.drain(..FRONT_THREADS.min(self.threads.len())) {
            let _ = t.join();
        }
    }
}

pub fn serve(addr: SocketAddr, svc: Arc<dyn Service>) -> std::io::Result<Running> {
    let server = Server::http(addr).map_err(|e| std::io::Error::other(e.to_string()))?;
    let bound = server.server_addr().to_ip().unwrap_or(addr);
    let server = Arc::new(server);
    let started = Instant::now();
    let (tx, rx) = sync_channel::<Mint>(MINT_QUEUE);
    let rx = Arc::new(Mutex::new(rx));
    let mut threads = Vec::new();
    for _ in 0..FRONT_THREADS {
        let (server, svc, tx) = (server.clone(), svc.clone(), tx.clone());
        threads.push(thread::spawn(move || front(&server, svc.as_ref(), &tx, started)));
    }
    drop(tx);
    for _ in 0..MINT_WORKERS {
        let (svc, rx) = (svc.clone(), rx.clone());
        threads.push(thread::spawn(move || worker(svc.as_ref(), &rx)));
    }
    Ok(Running { server, threads, addr: bound })
}

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("static header")
}

fn respond(req: Request, status: u16, body: String) {
    let r = Response::from_string(body).with_status_code(status);
    let r = if status == 204 { r } else { r.with_header(json_header()) };
    let _ = req.respond(r);
}

fn front(server: &Server, svc: &dyn Service, mints: &SyncSender<Mint>, started: Instant) {
    // `recv` errors once the server is unblocked for shutdown or its socket fails.
    while let Ok(mut req) = server.recv() {
        let method = match req.method() {
            Method::Get => "GET",
            Method::Post => "POST",
            _ => "OTHER",
        };
        let path = req.url().to_owned();
        let content_type = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Content-Type"))
            .map(|h| h.value.as_str().to_owned());
        let wants_body = method == "POST" && path.split('?').next() == Some("/get_pot");
        let mut body = Vec::new();
        if wants_body {
            if req.body_length().is_some_and(|n| n > MAX_BODY) {
                respond(req, 413, json!({ "error": "Request body too large" }).to_string());
                continue;
            }
            let _ = req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body);
        }
        let uptime = started.elapsed().as_secs_f64();
        match route(method, &path, content_type.as_deref(), &body, svc, uptime) {
            Routed::Reply(status, body) => respond(req, status, body),
            Routed::Mint(r) => match mints.try_send((req, r)) {
                Ok(()) => {}
                Err(TrySendError::Full((req, _))) | Err(TrySendError::Disconnected((req, _))) => {
                    respond(req, 503, json!({ "error": "Too many token requests in flight" }).to_string());
                }
            },
        }
    }
}

fn worker(svc: &dyn Service, queue: &Mutex<Receiver<Mint>>) {
    loop {
        let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok((req, r)) = job else { return };
        let (status, body) = mint_reply(svc.get_pot(&r));
        respond(req, status, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fake {
        fail: bool,
    }

    impl Service for Fake {
        fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String> {
            if self.fail {
                return Err("botguard said no".into());
            }
            Ok(PotAnswer {
                content_binding: r.content_binding.clone(),
                po_token: "tok".into(),
                expires_at: UNIX_EPOCH + Duration::from_millis(1_794_139_200_123),
            })
        }
        fn invalidate_caches(&self) {}
        fn invalidate_it(&self) {}
        fn minter_keys(&self) -> Vec<String> {
            vec!["default".into()]
        }
    }

    const JSON: Option<&str> = Some("application/json");

    fn post(body: &str, svc: &dyn Service) -> (u16, Value) {
        let (s, b) = handle("POST", "/get_pot", JSON, body.as_bytes(), svc, 1.0);
        (s, serde_json::from_str(&b).unwrap())
    }

    #[test]
    fn ping_names_the_protocol_and_the_implementation() {
        let (s, b) = handle("GET", "/ping", None, b"", &Fake { fail: false }, 12.5);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(s, 200);
        assert_eq!(v["version"], "2.0.0");
        assert_eq!(v["server_uptime"], 12.5);
        assert_eq!(v["implementation"], format!("streamloop-pot {}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn get_pot_refuses_anything_but_json() {
        let (s, b) = handle("POST", "/get_pot", Some("text/plain"), b"{}", &Fake { fail: false }, 1.0);
        assert_eq!(s, 415);
        assert!(b.contains("Content-Type must be application/json"));
        assert_eq!(handle("POST", "/get_pot", None, b"{}", &Fake { fail: false }, 1.0).0, 415);
    }

    #[test]
    fn get_pot_refuses_a_request_proxy_source_address_or_tls_off() {
        for extra in [r#""proxy":"http://x:1""#, r#""source_address":"1.2.3.4""#, r#""disable_tls_verification":true"#] {
            let (s, v) = post(&format!(r#"{{"content_binding":"a",{extra}}}"#), &Fake { fail: false });
            assert_eq!(s, 400, "{extra}");
            assert_eq!(v["error"], "proxy, source_address and disable_tls_verification are not supported");
        }
    }

    #[test]
    fn get_pot_accepts_the_nulls_the_plugin_sends() {
        let body = r#"{"content_binding":"vd","proxy":null,"source_address":null,"disable_tls_verification":false,"bypass_cache":false,"challenge":null,"innertube_context":null}"#;
        assert_eq!(post(body, &Fake { fail: false }).0, 200);
    }

    #[test]
    fn get_pot_answers_the_shape_the_plugin_reads() {
        let (s, v) = post(r#"{"content_binding":"vd"}"#, &Fake { fail: false });
        assert_eq!(s, 200);
        assert_eq!(v["contentBinding"], "vd");
        assert_eq!(v["poToken"], "tok");
        assert_eq!(v["expiresAt"], "2026-11-08T12:00:00.123Z");
    }

    #[test]
    fn a_service_error_is_500_with_error() {
        let (s, v) = post(r#"{"content_binding":"vd"}"#, &Fake { fail: true });
        assert_eq!(s, 500);
        assert_eq!(v["error"], "botguard said no");
    }

    #[test]
    fn invalidate_endpoints_answer_204() {
        for p in ["/invalidate_caches", "/invalidate_it"] {
            assert_eq!(handle("POST", p, None, b"", &Fake { fail: false }, 1.0), (204, String::new()));
        }
    }

    #[test]
    fn minter_cache_lists_the_keys() {
        let (s, b) = handle("GET", "/minter_cache", None, b"", &Fake { fail: false }, 1.0);
        assert_eq!((s, b.as_str()), (200, r#"["default"]"#));
    }

    #[test]
    fn an_oversized_body_is_413() {
        let big = format!(r#"{{"content_binding":"{}"}}"#, "a".repeat(MAX_BODY));
        assert_eq!(post(&big, &Fake { fail: false }).0, 413);
    }

    #[test]
    fn the_deprecated_fields_get_2_0_0s_messages() {
        let f = &Fake { fail: false };
        let (s, v) = post(r#"{"content_binding":"a","data_sync_id":"x"}"#, f);
        assert_eq!((s, v["error"].as_str().unwrap()), (400, "data_sync_id is deprecated, use content_binding instead"));
        let (_, v) = post(r#"{"visitor_data":"x"}"#, f);
        assert_eq!(v["error"], "visitor_data is deprecated, use content_binding instead");
        let (_, v) = post(r#"{"content_binding":"a","disable_innertube":true}"#, f);
        assert_eq!(v["error"], "disable_innertube is deprecated because the /Create endpoint doesn't work anymore");
    }

    #[test]
    fn a_missing_binding_is_400_unless_the_context_has_visitor_data() {
        let f = &Fake { fail: false };
        assert_eq!(post(r#"{}"#, f).0, 400);
        assert_eq!(post(r#"{"innertube_context":{"client":{}}}"#, f).0, 400);
        assert_eq!(post(r#"{"innertube_context":{"client":{"visitorData":"vd"}}}"#, f).0, 200);
    }

    #[test]
    fn bad_json_and_unknown_routes() {
        let f = &Fake { fail: false };
        assert_eq!(post("{nope", f).0, 400);
        assert_eq!(handle("OPTIONS", "/get_pot", JSON, b"", f, 1.0).0, 404);
        assert_eq!(handle("GET", "/nothing", None, b"", f, 1.0).0, 404);
    }
}
