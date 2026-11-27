mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderValue, Method, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use serde_json::Value;
use streamloop_control::api::{json_reply, router, router_with, AppState, JsonBody};
use support::{captured_logs, Rig};
use tower::ServiceExt;

const TOKEN: &str = "test-token";

/// The base routes never touch the rig's fakes or files, so its temp folder can go at once.
fn app() -> Router {
    let mut state = AppState::new(Rig::new().deps());
    state.token = TOKEN.to_string();
    let routes = Router::new()
        .route("/echo", post(|Extension(JsonBody(v)): Extension<JsonBody>| async move { json_reply(StatusCode::OK, &v) }))
        .route("/ping", get(|| async { json_reply(StatusCode::OK, &serde_json::json!({"ok": true})) }));
    router_with(Arc::new(state), routes, 7654)
}

struct Reply {
    status: StatusCode,
    content_type: String,
    location: Option<String>,
    body: String,
}

async fn call(app: Router, mut req: Request<Body>) -> Reply {
    req.headers_mut().entry(header::HOST).or_insert(HeaderValue::from_static("127.0.0.1:7654"));
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    let location = res.headers().get(header::LOCATION).map(|v| v.to_str().unwrap().to_string());
    let body = String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    Reply { status, content_type, location, body }
}

async fn call_raw(app: Router, req: Request<Body>) -> Reply {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    let body = String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).into_owned();
    Reply { status, content_type, location: None, body }
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
    let logs = captured_logs();
    // Act
    let r = call(app(), post_echo(Some("application/json"), Some(TOKEN), r#"{"key":live_TESTKEY}"#)).await;
    // Assert
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body, r#"{"error":"Invalid JSON"}"#);
    assert!(!r.body.contains("TESTKEY"));
    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("entity.parse.failed"), "{logged}");
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
async fn the_root_path_is_404() {
    // Act
    let r = call(app(), get_req("/")).await;
    // Assert
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.content_type, "text/html; charset=utf-8");
    assert_eq!(r.body, "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Error</title>\n</head>\n<body>\n<pre>Cannot GET /</pre>\n</body>\n</html>\n");
}

#[tokio::test]
async fn admin_paths_answer_404() {
    // Arrange
    let paths = ["/admin", "/admin/", "/admin/admin.js"];
    for path in paths {
        // Act
        let r = call(app(), get_req(path)).await;
        // Assert
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.content_type, "text/html; charset=utf-8", "{path}");
        assert!(r.body.contains(&format!("<pre>Cannot GET {path}</pre>")), "{path}: {}", r.body);
    }
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
async fn the_default_router_has_the_token_route() {
    // Arrange
    let app = router(Arc::new(AppState::new(Rig::new().deps())), 7654);
    // Act
    let r = call(app, get_req("/api/token")).await;
    // Assert
    let token = serde_json::from_str::<Value>(&r.body).unwrap()["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn a_wrong_method_on_an_existing_path_gets_the_express_404_page() {
    // Arrange
    let post_token = Request::builder().method(Method::POST).uri("/api/token").header("x-api-token", TOKEN).body(Body::empty()).unwrap();
    let delete_ping = Request::builder().method(Method::DELETE).uri("/api/ping").body(Body::empty()).unwrap();
    let put_ping = Request::builder().method(Method::PUT).uri("/api/ping").body(Body::empty()).unwrap();
    // Act
    let replies = [call(app(), post_token).await, call(app(), delete_ping).await, call(app(), put_ping).await];
    // Assert
    let texts = ["POST /api/token", "DELETE /api/ping", "PUT /api/ping"];
    for (r, text) in replies.iter().zip(texts) {
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{text}");
        assert_eq!(r.content_type, "text/html; charset=utf-8", "{text}");
        assert!(r.body.contains(&format!("<pre>Cannot {text}</pre>")), "{text}: {}", r.body);
    }
}

#[tokio::test]
async fn a_malformed_body_never_reaches_a_log_line_even_at_trace_level() {
    // Arrange
    let logs = captured_logs();
    let before = logs.0.lock().unwrap().len();
    // Act
    call(app(), post_echo(Some("application/json"), Some(TOKEN), r#"{"url":"https://discord.com/api/webhooks/1/SECRETHOOK","k":}"#)).await;
    call(app(), post_echo(Some("application/json"), Some(TOKEN), r#"{"url":"https://discord.com/api/webhooks/1/SECRETHOOK"}"#)).await;
    // Assert
    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logged[before..].contains("entity.parse.failed"), "the capture saw no parse failure: {logged}");
    assert!(!logged.contains("SECRETHOOK"), "{logged}");
}

#[tokio::test]
async fn refuses_a_rebinding_host_before_the_token_check() {
    // Arrange
    let req = Request::builder().uri("/api/token").header(header::HOST, "evil.example:7654").body(Body::empty()).unwrap();
    // Act
    let r = call(app(), req).await;
    // Assert
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body, r#"{"error":"Forbidden: requests from a browser are refused"}"#);
    assert_eq!(r.content_type, "application/json; charset=utf-8");
}

#[tokio::test]
async fn refuses_a_post_with_an_origin() {
    // Arrange
    let mut req = post_echo(Some("application/json"), Some(TOKEN), "{}");
    req.headers_mut().insert(header::ORIGIN, HeaderValue::from_static("http://evil.example"));
    // Act
    let r = call(app(), req).await;
    // Assert
    assert_eq!((r.status, r.body.as_str()), (StatusCode::FORBIDDEN, r#"{"error":"Forbidden: requests from a browser are refused"}"#));
}

#[tokio::test]
async fn refuses_a_missing_host_and_a_wrong_port_on_any_path() {
    // Arrange
    let bare = Request::builder().uri("/nowhere").body(Body::empty()).unwrap();
    let wrong_port = Request::builder().uri("/api/ping").header(header::HOST, "127.0.0.1:4417").body(Body::empty()).unwrap();
    // Act
    let mut no_host = Request::builder().uri("/nowhere").body(Body::empty()).unwrap();
    no_host.headers_mut().remove(header::HOST);
    let replies = [call_raw(app(), no_host).await, call(app(), wrong_port).await, call(app(), bare).await];
    // Assert
    assert_eq!(replies[0].status, StatusCode::FORBIDDEN);
    assert_eq!(replies[1].status, StatusCode::FORBIDDEN);
    assert_eq!(replies[2].status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn answers_loopback_requests_as_before() {
    // Act
    let by_ip = call(app(), Request::builder().uri("/api/ping").header(header::HOST, "127.0.0.1:7654").body(Body::empty()).unwrap()).await;
    let by_name = call(app(), Request::builder().uri("/api/ping").header(header::HOST, "LocalHost:7654").body(Body::empty()).unwrap()).await;
    // Assert
    assert_eq!((by_ip.status, by_ip.body.as_str()), (StatusCode::OK, r#"{"ok":true}"#));
    assert_eq!(by_name.status, StatusCode::OK);
}
