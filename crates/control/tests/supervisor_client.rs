use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use streamloop_common::api::{ControlCmd, ReloadRequest};
use streamloop_control::http::{FakeHttp, Http, HttpError, HttpRequest, HttpResponse};
use streamloop_control::supervisor_client::fake::{Call, FakeSupervisor};
use streamloop_control::supervisor_client::{ControlResult, ReloadResult, SupervisorApi, SupervisorClient};

const BASE: &str = "http://127.0.0.1:4417";

// Shape of the supervisor's status() in supervisor/src/main.rs (head merged with body).
const FULL_STATUS: &str = r#"{
  "state": "playing", "reason": null, "streamKeyProblem": null,
  "playing": { "index": 3, "id": "abc123", "title": "A video", "source": "file", "position": 12.345, "duration": 300, "spareReady": false },
  "timeline": 1234.5, "readAheadSeconds": 18.2, "cacheBytes": 1000,
  "downloading": { "index": 4, "percent": 40 },
  "encoder": { "alive": true, "restarts": 1, "generation": 2, "mediaTime": 99.1, "speed": 1.0, "cpuPercent": 12.5 },
  "cpu": { "encoder": 12.5, "feeder": 3, "system": 30 },
  "tokenServer": { "state": "running", "restarts": 0 },
  "recovery": { "step": "none", "retries": 0 },
  "watchdog": { "last": null }, "playlistCount": 50, "hold": null,
  "youtube": { "breaker": "closed", "retryAt": null, "level": 0, "failures": 0 }
}"#;

fn client_over(http: Arc<dyn Http>) -> SupervisorClient {
    SupervisorClient::new(http, None, Some(Duration::from_millis(50)))
}

fn answering(status: u16, body: &str) -> (Arc<FakeHttp>, SupervisorClient) {
    let http = Arc::new(FakeHttp::new());
    http.respond(BASE, status, body);
    let client = client_over(http.clone());
    (http, client)
}

fn failing(err: HttpError) -> SupervisorClient {
    let http = Arc::new(FakeHttp::new());
    http.route(BASE, move |_| Err(err.clone()));
    client_over(http)
}

fn sent_body(http: &FakeHttp, n: usize) -> String {
    String::from_utf8(http.requests()[n].body.clone().unwrap()).unwrap()
}

/// Never answers, so only the client's own timeout ends a call.
struct SilentHttp;

#[async_trait]
impl Http for SilentHttp {
    async fn send(&self, _req: HttpRequest) -> Result<HttpResponse, HttpError> {
        std::future::pending().await
    }
    async fn download(&self, _url: &str, _dest: &std::path::Path) -> Result<u64, HttpError> {
        std::future::pending().await
    }
}

// status

#[tokio::test]
async fn status_parses_a_full_status_sample() {
    // Arrange
    let (_http, client) = answering(200, FULL_STATUS);
    // Act
    let s = client.status().await.unwrap();
    // Assert
    assert_eq!(s.state.as_deref(), Some("playing"));
    assert_eq!(s.playing.unwrap().title, Some(Some("A video".to_string())));
    assert_eq!(s.encoder.unwrap().alive, Some(true));
    assert_eq!(s.youtube.unwrap().breaker.as_deref(), Some("closed"));
    assert_eq!(s.playlist_count, Some(50));
}

#[tokio::test]
async fn status_tolerates_missing_and_unknown_fields() {
    // Arrange
    let (_http, client) = answering(200, r#"{"state":"starting","somethingNew":1}"#);
    // Act
    let s = client.status().await.unwrap();
    // Assert
    assert_eq!(s.state.as_deref(), Some("starting"));
    assert!(s.encoder.is_none());
}

#[tokio::test]
async fn status_keeps_the_version_and_encoder_alive_when_another_field_changed_type() {
    // Arrange: a later supervisor that sends playlistCount as text and the encoder's restarts as an object
    let (_http, client) = answering(200, r#"{"state":"playing","version":"3.2.0","playlistCount":"fifty","encoder":{"alive":true,"restarts":{"n":1}}}"#);
    // Act
    let s = client.status().await;
    // Assert: it still answers, so the version rule can replace it
    let s = s.expect("a supervisor that answers counts as answering");
    assert_eq!(s.version.as_deref(), Some("3.2.0"));
    assert_eq!(s.encoder.and_then(|e| e.alive), Some(true));
}

#[tokio::test]
async fn status_of_a_changed_shape_without_a_version_or_encoder_reads_as_answering_with_neither() {
    // Arrange
    let (_http, client) = answering(200, r#"{"state":7,"version":3,"encoder":{"alive":"yes"}}"#);
    // Act
    let s = client.status().await.expect("a supervisor that answers counts as answering");
    // Assert
    assert_eq!((s.version, s.encoder.and_then(|e| e.alive)), (None, None));
}

#[tokio::test]
async fn status_is_none_when_the_connection_is_refused() {
    // Arrange
    let client = failing(HttpError::connect("ECONNREFUSED", "refused"));
    // Act
    let s = client.status().await;
    // Assert
    assert!(s.is_none());
}

#[tokio::test(start_paused = true)]
async fn status_is_none_when_the_supervisor_does_not_answer_in_time() {
    // Arrange
    let client = SupervisorClient::new(Arc::new(SilentHttp), None, None);
    // Act
    let s = client.status().await;
    // Assert
    assert!(s.is_none());
}

#[tokio::test]
async fn status_is_none_on_a_non_200_answer() {
    // Arrange
    let (_http, client) = answering(500, r#"{"error":"x"}"#);
    // Act
    let s = client.status().await;
    // Assert
    assert!(s.is_none());
}

#[tokio::test]
async fn status_is_none_when_the_body_is_not_json() {
    // Arrange
    let (_http, client) = answering(200, "<html>");
    // Act
    let s = client.status().await;
    // Assert
    assert!(s.is_none());
}

#[tokio::test]
async fn status_is_none_when_the_body_is_json_but_not_an_object() {
    // Arrange
    let (_a, null) = answering(200, "null");
    let (_b, array) = answering(200, "[]");
    // Act
    let results = (null.status().await, array.status().await);
    // Assert
    assert!(results.0.is_none() && results.1.is_none());
}

#[tokio::test]
async fn status_asks_for_the_status_path_with_a_get() {
    // Arrange
    let (http, client) = answering(200, "{}");
    // Act
    client.status().await;
    // Assert
    let req = &http.requests()[0];
    assert_eq!((req.method.as_str(), req.url.as_str(), req.timeout), ("GET", "http://127.0.0.1:4417/status", Some(Duration::from_millis(50))));
}

// control

#[tokio::test]
async fn control_posts_the_exact_body_for_jump() {
    // Arrange
    let (http, client) = answering(200, r#"{"accepted":true}"#);
    // Act
    let r = client.control(ControlCmd::Jump, Some(7), None, None).await;
    // Assert
    assert_eq!(r, ControlResult { ok: true, error: None, already_restarting: false });
    let req = &http.requests()[0];
    assert_eq!((req.method.as_str(), req.url.as_str()), ("POST", "http://127.0.0.1:4417/control"));
    assert_eq!(sent_body(&http, 0), r#"{"cmd":"jump","index":7}"#);
}

#[tokio::test]
async fn control_sends_the_video_id_with_a_jump() {
    // Arrange
    let (http, client) = answering(200, r#"{"accepted":true}"#);
    // Act
    client.control(ControlCmd::Jump, Some(7), None, Some("abc".into())).await;
    // Assert
    assert_eq!(sent_body(&http, 0), r#"{"cmd":"jump","index":7,"id":"abc"}"#);
}

#[tokio::test]
async fn control_omits_the_index_when_none_is_given() {
    // Arrange
    let (http, client) = answering(200, r#"{"accepted":true}"#);
    // Act
    client.control(ControlCmd::Next, None, None, None).await;
    // Assert
    assert_eq!(sent_body(&http, 0), r#"{"cmd":"next"}"#);
}

#[tokio::test]
async fn control_sends_the_caller_reason_with_restart_encoder() {
    // Arrange
    let (http, client) = answering(200, r#"{"accepted":true}"#);
    // Act
    client.control(ControlCmd::RestartEncoder, None, Some("Twitch shows the channel offline".into()), None).await;
    // Assert
    assert_eq!(sent_body(&http, 0), r#"{"cmd":"restart-encoder","reason":"Twitch shows the channel offline"}"#);
}

#[tokio::test]
async fn control_reports_already_restarting_for_a_pending_encoder_restart() {
    // Arrange
    let (_http, client) = answering(200, r#"{"accepted":true,"alreadyRestarting":true}"#);
    // Act
    let r = client.control(ControlCmd::RestartEncoder, None, None, None).await;
    // Assert
    assert_eq!(r, ControlResult { ok: true, error: None, already_restarting: true });
}

#[tokio::test]
async fn control_returns_the_error_text_on_a_400() {
    // Arrange
    let (_http, client) = answering(400, r#"{"error":"needs-stream-key: x"}"#);
    // Act
    let r = client.control(ControlCmd::Next, None, None, None).await;
    // Assert
    assert_eq!(r, ControlResult { ok: false, error: Some("needs-stream-key: x".into()), already_restarting: false });
}

#[tokio::test]
async fn control_names_the_status_when_the_error_body_has_no_error_field() {
    // Arrange
    let (_http, client) = answering(503, "<html>");
    // Act
    let r = client.control(ControlCmd::Next, None, None, None).await;
    // Assert
    assert_eq!(r.error.as_deref(), Some("HTTP 503"));
}

#[tokio::test]
async fn control_resolves_with_an_error_when_the_network_fails() {
    // Arrange
    let client = failing(HttpError::connect("ECONNREFUSED", "error sending request for url (http://127.0.0.1:4417/control)"));
    // Act
    let r = client.control(ControlCmd::Next, None, None, None).await;
    // Assert
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("fetch failed"));
}

#[tokio::test(start_paused = true)]
async fn control_resolves_with_an_error_when_the_supervisor_does_not_answer() {
    // Arrange
    let client = SupervisorClient::new(Arc::new(SilentHttp), None, None);
    // Act
    let r = client.control(ControlCmd::Next, None, None, None).await;
    // Assert
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("timeout"));
}

// reload and shutdown

#[tokio::test]
async fn reload_returns_restart_required() {
    // Arrange
    let (_http, client) = answering(200, r#"{"applied":true,"restartRequired":true}"#);
    // Act
    let r = client.reload(ReloadRequest::default()).await;
    // Assert
    assert_eq!(r, ReloadResult { ok: true, restart_required: true, error: None, code: None });
}

#[tokio::test]
async fn reload_sends_an_empty_body_unless_a_start_is_given() {
    // Arrange
    let (http, client) = answering(200, r#"{"applied":true}"#);
    let req = |playlist: Option<&str>, index, id: Option<&str>| ReloadRequest { playlist: playlist.map(String::from), index, id: id.map(String::from) };
    // Act
    client.reload(req(None, None, None)).await;
    client.reload(req(None, Some(3), None)).await;
    client.reload(req(None, Some(3), Some("abc"))).await;
    client.reload(req(Some("PLb"), Some(3), None)).await;
    // Assert
    let bodies: Vec<String> = (0..4).map(|n| sent_body(&http, n)).collect();
    assert_eq!(bodies, [r#"{}"#, r#"{"index":3}"#, r#"{"index":3,"id":"abc"}"#, r#"{"playlist":"PLb","index":3}"#]);
    assert!(http.requests().iter().all(|r| r.method == "POST" && r.url == "http://127.0.0.1:4417/reload"));
}

#[tokio::test]
async fn reload_returns_the_error_from_a_rejected_reload() {
    // Arrange
    let (_http, client) = answering(400, r#"{"error":"bad config"}"#);
    // Act
    let r = client.reload(ReloadRequest::default()).await;
    // Assert
    assert_eq!(r, ReloadResult { ok: false, restart_required: false, error: Some("bad config".into()), code: None });
}

#[tokio::test]
async fn reload_keeps_the_code_of_a_refused_connection() {
    // Arrange
    let client = failing(HttpError::connect("ECONNREFUSED", "error sending request"));
    // Act
    let r = client.reload(ReloadRequest::default()).await;
    // Assert
    assert_eq!(r, ReloadResult { ok: false, restart_required: false, error: Some("fetch failed".into()), code: Some("ECONNREFUSED".into()) });
}

#[tokio::test]
async fn reload_reports_a_refused_connection_to_a_closed_local_port_through_the_real_http() {
    // Arrange
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let client = SupervisorClient::new(Arc::new(streamloop_control::http::ReqwestHttp::new()), Some(&format!("http://127.0.0.1:{port}")), Some(Duration::from_secs(10)));
    // Act
    let r = client.reload(ReloadRequest::default()).await;
    // Assert
    assert_eq!(r.code.as_deref(), Some("ECONNREFUSED"));
}

#[tokio::test]
async fn shutdown_is_true_on_200_and_false_on_failure() {
    // Arrange
    let (http, ok) = answering(200, "{}");
    let broken = failing(HttpError::other("x"));
    // Act
    let results = (ok.shutdown().await, broken.shutdown().await);
    // Assert
    assert_eq!(results, (true, false));
    assert_eq!(sent_body(&http, 0), "{}");
}

// wire parity with the Node backend

#[tokio::test]
async fn bodies_equal_the_ones_node_sent_the_fake_supervisor() {
    // Arrange
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../supervisor/scripts/api-parity/node.json");
    let recording: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let expected: Vec<(String, serde_json::Value)> = recording["supervisorRequests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["path"].as_str().unwrap().to_string(), r["body"].clone()))
        .collect();
    let (http, client) = answering(200, "{}");
    let reload = |playlist: Option<&str>, index, id: Option<&str>| ReloadRequest { playlist: playlist.map(String::from), index, id: id.map(String::from) };
    // Act
    for _ in 0..4 {
        client.reload(ReloadRequest::default()).await;
    }
    for cmd in [ControlCmd::Next, ControlCmd::Prev, ControlCmd::Restart] {
        client.control(cmd, None, None, None).await;
    }
    client.control(ControlCmd::Jump, Some(3), None, None).await;
    client.control(ControlCmd::Jump, Some(3), None, Some("abc123XYZ_-".into())).await;
    client.control(ControlCmd::Jump, Some(777), None, None).await;
    client.reload(reload(Some("PLparity000000002"), None, None)).await;
    client.reload(reload(Some("PLparity000000001"), Some(4), Some("abc123XYZ_-"))).await;
    client.reload(reload(Some("PLparity000000001"), Some(777), None)).await;
    client.shutdown().await;
    // Assert
    let sent: Vec<(String, serde_json::Value)> = http
        .requests()
        .iter()
        .map(|r| (r.url.strip_prefix(BASE).unwrap().to_string(), serde_json::from_slice(r.body.as_ref().unwrap()).unwrap()))
        .collect();
    assert_eq!(sent, expected);
}

// the fake

#[tokio::test(start_paused = true)]
async fn fake_supervisor_records_calls_and_answers_what_was_set() {
    // Arrange
    let fake = FakeSupervisor::new();
    fake.set_control_result(ControlResult { ok: false, error: Some("no".into()), already_restarting: false });
    // Act
    let status = fake.status().await;
    let control = fake.control(ControlCmd::Next, None, None, None).await;
    // Assert
    assert!(status.is_none());
    assert_eq!(control.error.as_deref(), Some("no"));
    assert_eq!(fake.calls(), [Call::Status, Call::Control { cmd: ControlCmd::Next, index: None, reason: None, id: None }]);
}

#[tokio::test(start_paused = true)]
async fn fake_supervisor_hangs_until_released() {
    // Arrange
    let fake = Arc::new(FakeSupervisor::new());
    fake.hang();
    let call = tokio::spawn({
        let fake = fake.clone();
        async move { fake.shutdown().await }
    });
    // Act
    tokio::time::sleep(Duration::from_secs(5)).await;
    let finished_while_hanging = call.is_finished();
    fake.release();
    let answer = call.await.unwrap();
    // Assert
    assert!(!finished_while_hanging);
    assert!(answer);
}
