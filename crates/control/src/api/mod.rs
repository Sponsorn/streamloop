//! The router base: what Express did around the routes in `src/server/index.ts` and `api.ts`. JSON body parsing,
//! the API token check, the static `/admin` files and Express's own 404 page. Route handlers hang off `router_with`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{OriginalUri, Path, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;
use serde_json::{json, Value};

/// Shared by every handler. Later tasks add their components here.
pub struct AppState {
    /// 32 random bytes as hex, new per process start. Every POST must send it as `x-api-token`.
    pub token: String,
}

impl AppState {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        Self { token: bytes.iter().map(|b| format!("{b:02x}")).collect() }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// The parsed request body, `{}` unless the request carried JSON. Handlers take it as `Extension<JsonBody>`.
#[derive(Clone)]
pub struct JsonBody(pub Value);

/// `res.json()`: compact JSON as `application/json; charset=utf-8`.
pub fn json_reply(status: StatusCode, body: &Value) -> Response {
    let mut res = (status, body.to_string()).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    res
}

// express.json's default limit.
const BODY_LIMIT: usize = 100 * 1024;

#[derive(RustEmbed)]
#[folder = "../../src/admin/"]
struct AdminFiles;

pub fn router(state: Arc<AppState>) -> Router {
    router_with(state, Router::new())
}

/// The base router with `api_routes` mounted under `/api`, behind the body parser and the token check.
pub fn router_with(state: Arc<AppState>, api_routes: Router<Arc<AppState>>) -> Router {
    let api = Router::new()
        .route("/token", get(token))
        .merge(api_routes)
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), api_guard));
    Router::new()
        .nest("/api", api)
        .route("/admin", get(admin_redirect))
        .route("/admin/", get(admin_index))
        .route("/admin/{*path}", get(admin_file))
        .fallback(not_found)
        .with_state(state)
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

async fn admin_redirect() -> Response {
    let mut res = express_page(StatusCode::MOVED_PERMANENTLY, "text/html; charset=UTF-8", "Redirecting", "Redirecting to /admin/");
    res.headers_mut().insert(header::LOCATION, HeaderValue::from_static("/admin/"));
    res
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=UTF-8",
        "css" => "text/css; charset=UTF-8",
        "js" | "mjs" => "text/javascript; charset=UTF-8",
        "json" => "application/json; charset=UTF-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn serve_admin(name: &str) -> Option<Response> {
    if name.is_empty() || name.contains("..") || name.contains('\\') {
        return None;
    }
    let file = AdminFiles::get(name)?;
    let mut res = (StatusCode::OK, file.data.into_owned()).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(name)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=0"));
    Some(res)
}

async fn admin_index(method: Method, uri: OriginalUri) -> Response {
    match serve_admin("index.html") {
        Some(r) => r,
        None => not_found(method, uri).await,
    }
}

async fn admin_file(Path(name): Path<String>, method: Method, uri: OriginalUri) -> Response {
    match serve_admin(&name) {
        Some(r) => r,
        None => not_found(method, uri).await,
    }
}
