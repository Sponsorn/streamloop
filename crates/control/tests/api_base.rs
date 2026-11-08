use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use serde_json::Value;
use streamloop_control::api::{json_reply, router, router_with, AppState, JsonBody};
use tower::ServiceExt;

const TOKEN: &str = "test-token";

fn app() -> Router {
    let state = Arc::new(AppState { token: TOKEN.to_string() });
    let routes = Router::new()
        .route("/echo", post(|Extension(JsonBody(v)): Extension<JsonBody>| async move { json_reply(StatusCode::OK, &v) }))
        .route("/ping", get(|| async { json_reply(StatusCode::OK, &serde_json::json!({"ok": true})) }));
    router_with(state, routes)
}

struct Reply {
    status: StatusCode,
    content_type: String,
    location: Option<String>,
    body: String,
}

async fn call(app: Router, req: Request<Body>) -> Reply {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    let location = res.headers().get(header::LOCATION).map(|v| v.to_str().unwrap().to_string());
    let body = String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    Reply { status, content_type, location, body }
}

fn post_echo(content_type: Option<&str>, token: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder().method(Method::POST).uri("/api/echo");
    if let Some(ct) = content_type {
        b = b.header(header::CONTENT_TYPE, ct);
    }
    if let Some(t) = token {
        b = b.header("x-api-token", t);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

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

#[tokio::test]
async fn post_without_the_token_is_forbidden() {
    // Act
    let r = call(app(), post_echo(Some("application/json"), None, "{}")).await;
    // Assert
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body, r#"{"error":"Forbidden: invalid API token"}"#);
    assert_eq!(r.content_type, "application/json; charset=utf-8");
}

#[tokio::test]
async fn post_with_a_wrong_token_is_forbidden() {
    // Act
    let r = call(app(), post_echo(Some("application/json"), Some("nope"), "{}")).await;
    // Assert
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn post_with_the_token_reaches_the_route_with_the_parsed_body() {
    // Act
    let r = call(app(), post_echo(Some("application/json"), Some(TOKEN), r#"{"a":1}"#)).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, r#"{"a":1}"#);
}

#[tokio::test]
async fn an_unknown_api_post_without_the_token_is_forbidden_not_missing() {
    // Act
    let r = call(app(), Request::builder().method(Method::POST).uri("/api/nowhere").body(Body::empty()).unwrap()).await;
    // Assert
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_needs_no_token() {
    // Act
    let r = call(app(), get_req("/api/ping")).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, r#"{"ok":true}"#);
}

#[tokio::test]
async fn the_token_route_hands_out_the_process_token() {
    // Act
    let r = call(app(), get_req("/api/token")).await;
    // Assert
    assert_eq!(r.body, r#"{"token":"test-token"}"#);
}

#[tokio::test]
async fn malformed_json_is_400_and_never_echoed() {
    // Arrange
    let logs = Capture::default();
    let subscriber = tracing_subscriber::fmt().with_writer(logs.clone()).with_ansi(false).finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Act
    let r = call(app(), post_echo(Some("application/json"), Some(TOKEN), r#"{"key":live_TESTKEY}"#)).await;
    // Assert
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body, r#"{"error":"Invalid JSON"}"#);
    assert!(!r.body.contains("TESTKEY"));
    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("API request failed"), "{logged}");
    assert!(!logged.contains("TESTKEY"), "{logged}");
}

#[tokio::test]
async fn a_json_scalar_is_refused_as_invalid_json() {
    // Act
    let r = call(app(), post_echo(Some("application/json"), Some(TOKEN), "123")).await;
    // Assert
    assert_eq!((r.status, r.body.as_str()), (StatusCode::BAD_REQUEST, r#"{"error":"Invalid JSON"}"#));
}

#[tokio::test]
async fn a_body_over_the_limit_is_an_internal_error_that_does_not_echo_it() {
    // Arrange
    let big = format!("{{\"k\":\"{}\"}}", "x".repeat(200 * 1024));
    // Act
    let r = call(app(), post_echo(Some("application/json"), Some(TOKEN), &big)).await;
    // Assert
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(r.body, r#"{"error":"Internal error"}"#);
}

#[tokio::test]
async fn a_body_without_a_json_content_type_is_empty() {
    // Act
    let plain = call(app(), post_echo(Some("text/plain"), Some(TOKEN), r#"{"a":1}"#)).await;
    let none = call(app(), post_echo(None, Some(TOKEN), r#"{"a":1}"#)).await;
    let empty_json = call(app(), post_echo(Some("application/json"), Some(TOKEN), "")).await;
    // Assert
    assert_eq!(plain.body, "{}");
    assert_eq!(none.body, "{}");
    assert_eq!(empty_json.body, "{}");
}

#[tokio::test]
async fn a_json_content_type_with_a_charset_still_parses() {
    // Act
    let r = call(app(), post_echo(Some("Application/JSON; charset=utf-8"), Some(TOKEN), r#"{"a":[1]}"#)).await;
    // Assert
    assert_eq!(r.body, r#"{"a":[1]}"#);
}

#[tokio::test]
async fn admin_redirects_to_admin_slash() {
    // Act
    let r = call(app(), get_req("/admin")).await;
    // Assert
    assert_eq!(r.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(r.location.as_deref(), Some("/admin/"));
    assert_eq!(r.content_type, "text/html; charset=UTF-8");
    assert_eq!(
        r.body,
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Redirecting</title>\n</head>\n<body>\n<pre>Redirecting to /admin/</pre>\n</body>\n</html>\n"
    );
}

#[tokio::test]
async fn admin_slash_serves_index_html() {
    // Act
    let r = call(app(), get_req("/admin/")).await;
    let direct = call(app(), get_req("/admin/index.html")).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "text/html; charset=UTF-8");
    assert!(r.body.contains("<html"));
    assert_eq!(r.body, direct.body);
}

#[tokio::test]
async fn scripts_go_out_as_text_javascript() {
    // Act
    let r = call(app(), get_req("/admin/admin.js")).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "text/javascript; charset=UTF-8");
}

#[tokio::test]
async fn styles_go_out_as_text_css() {
    // Act
    let r = call(app(), get_req("/admin/admin.css")).await;
    // Assert
    assert_eq!((r.status, r.content_type.as_str()), (StatusCode::OK, "text/css; charset=UTF-8"));
}

#[tokio::test]
async fn the_root_path_is_404() {
    // Act
    let r = call(app(), get_req("/")).await;
    // Assert
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.content_type, "text/html; charset=utf-8");
    assert_eq!(r.body, "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot GET /</pre>\n</body>\n</html>\n");
}

#[tokio::test]
async fn an_unknown_admin_file_is_the_same_404_page_naming_the_path() {
    // Act
    let r = call(app(), get_req("/admin/nope.js?x=1")).await;
    // Assert
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.body.contains("<pre>Cannot GET /admin/nope.js</pre>"), "{}", r.body);
}

#[tokio::test]
async fn a_path_that_climbs_out_of_the_admin_folder_is_404() {
    // Act
    let r = call(app(), get_req("/admin/..%2f..%2fCargo.toml")).await;
    // Assert
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unmatched_route_with_the_token_answers_the_post_404_page() {
    // Act
    let r = call(app(), Request::builder().method(Method::POST).uri("/api/%3Cx%3E").header("x-api-token", TOKEN).body(Body::empty()).unwrap()).await;
    // Assert
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.body.contains("<pre>Cannot POST /api/%3Cx%3E</pre>"), "{}", r.body);
}

#[tokio::test]
async fn the_default_router_has_the_token_and_admin_routes() {
    // Arrange
    let app = router(Arc::new(AppState::new()));
    // Act
    let r = call(app, get_req("/api/token")).await;
    // Assert
    let token = serde_json::from_str::<Value>(&r.body).unwrap()["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
}
