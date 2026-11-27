use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;
use streamloop_common::config::ordered::Value as Json;
use streamloop_common::config::{self, Discord};
use streamloop_control::clock::TestClock;
use streamloop_control::discord::{render_template, DiscordNotifier, EmbedField, Level, Notifier};
use streamloop_control::events::{Alert, AlertLevel, DiscordKind};
use streamloop_control::http::{FakeHttp, Http, HttpError, HttpRequest, HttpResponse};

const HOOK: &str = "https://discord.com/api/webhooks/test";

fn discord(overrides: serde_json::Value) -> Discord {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let text = json!({ "playlists": [{ "id": "PL1" }], "discord": overrides }).to_string();
    std::fs::write(&path, text).unwrap();
    config::read(&path).unwrap().discord
}

fn hooked(overrides: serde_json::Value) -> Discord {
    let mut o = overrides;
    if o.get("webhookUrl").is_none() {
        o["webhookUrl"] = HOOK.into();
    }
    discord(o)
}

fn notifier_with(d: Discord, http: &Arc<FakeHttp>) -> Arc<DiscordNotifier> {
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_792_929_600)));
    Arc::new(DiscordNotifier::new(d, "1.0.0", Arc::new(|| 3_600_000), http.clone(), clock))
}

fn ok_http() -> Arc<FakeHttp> {
    let http = Arc::new(FakeHttp::new());
    http.respond("https://", 200, "");
    http
}

fn body(http: &FakeHttp, i: usize) -> serde_json::Value {
    serde_json::from_slice(http.requests()[i].body.as_ref().unwrap()).unwrap()
}

/// Moves time on after the batch timer's task has had a turn to start its sleep, then lets it run.
async fn pass(d: Duration) {
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(d).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn does_nothing_when_the_webhook_url_is_empty() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(discord(json!({})), &http);
    // Act
    n.send("test message", Level::Info, None).await;
    pass(Duration::from_secs(60)).await;
    // Assert
    assert!(http.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn sends_a_webhook_after_the_batch_window() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("Hello", Level::Info, None).await;
    let before = http.requests().len();
    pass(Duration::from_secs(30)).await;
    // Assert
    assert_eq!(before, 0);
    assert_eq!(http.requests().len(), 1);
    assert_eq!(body(&http, 0)["embeds"][0]["description"], "Hello");
}

#[tokio::test(start_paused = true)]
async fn sends_nothing_before_the_five_second_window_ends() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("Hello", Level::Info, None).await;
    pass(Duration::from_millis(4900)).await;
    let early = http.requests().len();
    pass(Duration::from_millis(200)).await;
    // Assert
    assert_eq!((early, http.requests().len()), (0, 1));
}

#[tokio::test(start_paused = true)]
async fn flushes_immediately_on_error_level() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.notify_critical("All steps exhausted").await;
    // Assert
    assert_eq!(http.requests().len(), 1);
    assert!(body(&http, 0)["embeds"][0]["description"].as_str().unwrap().contains("CRITICAL"));
}

#[tokio::test(start_paused = true)]
async fn batches_multiple_messages_into_one_embed() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("First", Level::Info, None).await;
    n.send("Second", Level::Warn, None).await;
    let before = http.requests().len();
    pass(Duration::from_secs(30)).await;
    // Assert
    assert_eq!((before, http.requests().len()), (0, 1));
    let b = body(&http, 0);
    let description = b["embeds"][0]["description"].as_str().unwrap();
    assert!(description.contains("First") && description.contains("Second"));
    assert!(b["embeds"][0]["footer"]["text"].as_str().unwrap().contains("2 events"));
    assert_eq!(b["embeds"][0]["color"], 16_776_960);
}

#[tokio::test(start_paused = true)]
async fn an_error_message_flushes_the_queued_messages_too() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("queued info", Level::Info, None).await;
    n.send("queued warn", Level::Warn, None).await;
    n.notify_critical("urgent").await;
    pass(Duration::from_secs(30)).await;
    // Assert
    assert_eq!(http.requests().len(), 1);
    let b = body(&http, 0);
    let description = b["embeds"][0]["description"].as_str().unwrap();
    assert!(description.contains("queued info") && description.contains("urgent"));
    assert!(b["embeds"][0]["footer"]["text"].as_str().unwrap().contains("3 events"));
}

#[tokio::test(start_paused = true)]
async fn survives_a_failed_post() {
    // Arrange
    let http = Arc::new(FakeHttp::new());
    http.route("https://", |_| Err(HttpError::connect("ECONNREFUSED", "network")));
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("test", Level::Error, None).await;
    // Assert
    assert_eq!(http.requests().len(), 1);
}

#[test]
fn renders_a_template_with_variables() {
    // Arrange
    let vars = vec![("errorCode".to_string(), json!(150)), ("videoIndex".to_string(), json!(3))];
    // Act
    let out = render_template("Error {errorCode} on video #{videoIndex}", &vars);
    // Assert
    assert_eq!(out, "Error 150 on video #3");
}

#[test]
fn leaves_unknown_placeholders_intact() {
    // Arrange
    let vars = vec![("name".to_string(), json!("world"))];
    // Act
    let out = render_template("Hello {name}, {unknown}", &vars);
    // Assert
    assert_eq!(out, "Hello world, {unknown}");
}

#[test]
fn a_brace_that_is_not_a_placeholder_stays_and_a_later_placeholder_still_renders() {
    // Arrange
    let vars = vec![("a".to_string(), json!("x"))];
    // Act
    let out = render_template("{ {a} {} {a-b} {{a}", &vars);
    // Assert
    assert_eq!(out, "{ x {} {a-b} {x");
}

#[tokio::test(start_paused = true)]
async fn includes_bot_identity_in_the_payload_when_set() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({ "botName": "MyBot", "avatarUrl": "https://example.com/avatar.png" })), &http);
    // Act
    n.send("test", Level::Error, None).await;
    // Assert
    let b = body(&http, 0);
    assert_eq!(b["username"], "MyBot");
    assert_eq!(b["avatar_url"], "https://example.com/avatar.png");
}

#[tokio::test(start_paused = true)]
async fn leaves_out_bot_identity_when_empty() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("test", Level::Error, None).await;
    // Assert
    let b = body(&http, 0);
    assert!(b.get("username").is_none() && b.get("avatar_url").is_none());
}

#[tokio::test(start_paused = true)]
async fn alerts_carry_no_dashboard_line() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("test", Level::Error, None).await;
    // Assert
    let sent = body(&http, 0).to_string();
    assert!(!sent.contains("Dashboard:"), "{sent}");
}

#[tokio::test(start_paused = true)]
async fn includes_the_uptime_and_version_in_the_footer() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.send("test", Level::Error, None).await;
    // Assert
    let footer = body(&http, 0)["embeds"][0]["footer"]["text"].as_str().unwrap().to_string();
    assert_eq!(footer, "Uptime: 1h 0m | v1.0.0");
}

#[tokio::test(start_paused = true)]
async fn a_critical_carries_a_status_field_and_a_ram_field() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.notify_critical("All recovery steps exhausted").await;
    // Assert
    let fields = body(&http, 0)["embeds"][0]["fields"].clone();
    assert_eq!(fields[0]["name"], "Status");
    assert_eq!(fields[1]["name"], "RAM");
}

#[tokio::test(start_paused = true)]
async fn adds_a_role_ping_on_a_critical() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({ "rolePing": "123456789" })), &http);
    // Act
    n.notify_critical("All steps exhausted").await;
    // Assert
    assert_eq!(body(&http, 0)["content"], "<@&123456789>");
}

#[tokio::test(start_paused = true)]
async fn adds_no_role_ping_on_a_non_critical_message() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({ "rolePing": "123456789" })), &http);
    // Act
    n.send("info message", Level::Info, None).await;
    pass(Duration::from_secs(30)).await;
    // Assert
    assert!(body(&http, 0).get("content").is_none());
}

#[tokio::test(start_paused = true)]
async fn sends_a_described_event_with_its_template_and_level() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    let alert = Alert { kind: DiscordKind::Critical, vars: vec![("message".into(), json!("All videos failed"))], level: AlertLevel::Error };
    // Act
    n.notify_event(alert).await;
    // Assert
    let b = body(&http, 0);
    assert_eq!(b["embeds"][0]["description"], "**CRITICAL:** All videos failed");
    assert_eq!(b["embeds"][0]["color"], 15_158_332);
}

#[tokio::test(start_paused = true)]
async fn skips_a_described_event_whose_toggle_is_off() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({ "events": { "skip": false } })), &http);
    let alert = Alert {
        kind: DiscordKind::Skip,
        vars: vec![("videoIndex".into(), json!(1)), ("videoId".into(), json!("x")), ("reason".into(), json!("r"))],
        level: AlertLevel::Warn,
    };
    // Act
    n.notify_event(alert).await;
    pass(Duration::from_secs(30)).await;
    // Assert
    assert!(http.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn flush_sends_the_batch_now_and_leaves_the_timer_with_nothing() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    n.send("one", Level::Info, None).await;
    // Act
    n.flush().await;
    let after_flush = http.requests().len();
    pass(Duration::from_secs(30)).await;
    // Assert
    assert_eq!((after_flush, http.requests().len()), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn flush_with_nothing_queued_posts_nothing() {
    // Arrange
    let http = ok_http();
    let n = notifier_with(hooked(json!({})), &http);
    // Act
    n.flush().await;
    // Assert
    assert!(http.requests().is_empty());
}

#[tokio::test(start_paused = true)]
async fn drops_the_batch_after_a_429_wait() {
    // Arrange
    let http = Arc::new(FakeHttp::new());
    http.route("https://", |_| {
        let mut res = HttpResponse::new(429, "");
        res.headers.push(("Retry-After".into(), "2".into()));
        Ok(res)
    });
    let n = notifier_with(hooked(json!({})), &http);
    n.send("queued", Level::Info, None).await;
    let task = {
        let n = n.clone();
        tokio::spawn(async move { n.send("urgent", Level::Error, None).await })
    };
    // Act
    pass(Duration::from_millis(1900)).await;
    let waiting = !task.is_finished();
    pass(Duration::from_millis(200)).await;
    let finished = task.is_finished();
    pass(Duration::from_secs(60)).await;
    // Assert
    assert!(waiting && finished);
    assert_eq!(http.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_message_after_a_429_wait_goes_out_as_a_new_batch() {
    // Arrange
    let http = Arc::new(FakeHttp::new());
    let calls = Arc::new(Mutex::new(0));
    let seen = calls.clone();
    http.route("https://", move |_| {
        let mut n = seen.lock().unwrap();
        *n += 1;
        if *n == 1 {
            let mut res = HttpResponse::new(429, "");
            res.headers.push(("retry-after".into(), "1".into()));
            Ok(res)
        } else {
            Ok(HttpResponse::new(200, ""))
        }
    });
    let n = notifier_with(hooked(json!({})), &http);
    n.send("lost", Level::Error, None).await;
    // Act
    n.send("kept", Level::Error, None).await;
    // Assert
    assert_eq!(http.requests().len(), 2);
    assert_eq!(body(&http, 1)["embeds"][0]["description"], "kept");
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

/// One process-wide capture at TRACE, as in api_base.rs: a per-test subscriber can be blinded by tracing's call-site cache.
fn captured_logs() -> &'static Capture {
    static LOGS: OnceLock<Capture> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Capture::default();
        let subscriber = tracing_subscriber::fmt().with_writer(logs.clone()).with_ansi(false).with_max_level(tracing::Level::TRACE).finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other test installs a global subscriber");
        logs
    })
}

#[tokio::test(start_paused = true)]
async fn the_webhook_url_never_reaches_a_log_line() {
    // Arrange
    let logs = captured_logs();
    let secret = "https://discord.example/api/webhooks/123/SECRETTOKEN";
    let http = Arc::new(FakeHttp::new());
    let step = Arc::new(Mutex::new(0));
    let counter = step.clone();
    http.route("https://", move |req| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        match *n {
            1 => Err(HttpError::other(format!("error sending request for url ({})", req.url))),
            2 => Ok(HttpResponse::new(500, "")),
            _ => {
                let mut res = HttpResponse::new(429, "");
                res.headers.push(("retry-after".into(), "1".into()));
                Ok(res)
            }
        }
    });
    let n = notifier_with(hooked(json!({ "webhookUrl": secret })), &http);
    // Act
    for _ in 0..3 {
        n.send("x", Level::Error, None).await;
    }
    // Assert
    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("Discord webhook error") && logged.contains("Discord webhook failed") && logged.contains("rate limited"));
    assert!(!logged.contains("SECRETTOKEN") && !logged.contains("discord.example"));
}

// ---------------------------------------------------------------- the recorded Node cases

fn discord_kind(name: &str) -> DiscordKind {
    [
        DiscordKind::Error,
        DiscordKind::Skip,
        DiscordKind::Recovery,
        DiscordKind::Critical,
        DiscordKind::Resume,
        DiscordKind::StreamDrop,
        DiscordKind::StreamRestart,
        DiscordKind::OutputFrozen,
        DiscordKind::TwitchMismatch,
        DiscordKind::TwitchRestart,
    ]
    .into_iter()
    .find(|k| k.as_str() == name)
    .unwrap_or_else(|| panic!("unknown kind {name}"))
}

fn level_of(name: &str) -> Level {
    match name {
        "info" => Level::Info,
        "warn" => Level::Warn,
        "error" => Level::Error,
        other => panic!("unknown level {other}"),
    }
}

fn text(v: Option<&Json>) -> String {
    match v {
        Some(Json::String(s)) => s.clone(),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn to_serde(v: &Json) -> serde_json::Value {
    serde_json::from_str(&v.to_compact()).unwrap()
}

/// The same blanking the recorder applied: timestamps and the machine's memory.
fn blank(v: &mut Json) {
    match v {
        Json::Object(o) => {
            let is_ram = o.iter().any(|(k, v)| k == "name" && *v == Json::String("RAM".into()));
            for (k, child) in o.iter_mut() {
                if k == "timestamp" && matches!(child, Json::String(_)) {
                    *child = Json::String("<timestamp>".into());
                } else if k == "value" && is_ram {
                    *child = Json::String("<memory>".into());
                } else {
                    blank(child);
                }
            }
        }
        Json::Array(a) => a.iter_mut().for_each(blank),
        _ => {}
    }
}

async fn run_case(input: &Json) -> Arc<FakeHttp> {
    let overrides = to_serde(input.get("discord").unwrap());
    let uptime_ms = match input.get("uptimeMs") {
        Some(Json::Number(n)) => *n as u64,
        _ => panic!("uptimeMs"),
    };
    let http = ok_http();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_792_929_600)));
    let n = DiscordNotifier::new(
        discord(overrides),
        &text(input.get("appVersion")),
        Arc::new(move || uptime_ms),
        http.clone(),
        clock,
    );
    let Some(Json::Array(steps)) = input.get("steps") else { panic!("steps") };
    for step in steps {
        match text(step.get("call")).as_str() {
            "send" => {
                let level = level_of(&step.get("level").map_or("info".to_string(), |l| text(Some(l))));
                let fields = match step.get("fields") {
                    Some(Json::Array(fs)) => Some(
                        fs.iter()
                            .map(|f| EmbedField {
                                name: text(f.get("name")),
                                value: text(f.get("value")),
                                inline: match f.get("inline") {
                                    Some(Json::Bool(b)) => Some(*b),
                                    _ => None,
                                },
                            })
                            .collect(),
                    ),
                    _ => None,
                };
                n.send(&text(step.get("content")), level, fields).await;
            }
            "notifyCritical" => n.notify_critical(&text(step.get("message"))).await,
            "notifyTwitchMismatch" => n.notify_twitch_mismatch(&text(step.get("channel"))).await,
            "notifyTwitchRestart" => n.notify_twitch_restart(&text(step.get("channel"))).await,
            "notifyEvent" => {
                let a = step.get("alert").unwrap();
                let serde_json::Value::Object(vars) = to_serde(a.get("vars").unwrap()) else { panic!("vars") };
                let Some(Json::Object(ordered)) = a.get("vars") else { panic!("vars") };
                let vars = ordered.iter().map(|(k, _)| (k.clone(), vars[k].clone())).collect();
                n.notify_event(Alert { kind: discord_kind(&text(a.get("kind"))), vars, level: level_of(&text(a.get("level"))) }).await;
            }
            "flush" => n.flush().await,
            other => panic!("unknown call {other}"),
        }
    }
    n.flush().await;
    http
}

#[tokio::test(start_paused = true)]
async fn matches_every_discord_fixture() {
    // Arrange
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/discord.json")).unwrap();
    let Json::Array(cases) = Json::parse(&raw).unwrap() else { panic!("fixture is an array") };
    assert_eq!(cases.len(), 35);
    for case in &cases {
        let name = text(case.get("name"));
        // Act
        let http = run_case(case.get("input").unwrap()).await;
        // Assert
        let Some(Json::Array(expected)) = case.get("output") else { panic!("output") };
        let sent = http.requests();
        assert_eq!(sent.len(), expected.len(), "{name}: number of posts");
        for (req, want) in sent.iter().zip(expected) {
            assert_eq!(req.url, text(want.get("url")), "{name}: url");
            assert_eq!(req.method, text(want.get("method")), "{name}: method");
            let Some(Json::Object(headers)) = want.get("headers") else { panic!("headers") };
            let sent_headers: Vec<(String, String)> = req.headers.clone();
            let want_headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.clone(), text(Some(v)))).collect();
            assert_eq!(sent_headers, want_headers, "{name}: headers");
            let mut got = Json::parse(std::str::from_utf8(req.body.as_ref().unwrap()).unwrap()).unwrap();
            blank(&mut got);
            let mut want_body = want.get("body").unwrap().clone();
            blank(&mut want_body);
            assert_eq!(got.to_compact(), want_body.to_compact(), "{name}: body");
        }
    }
}

struct GatedHttp {
    gate: tokio::sync::Notify,
    posts: Mutex<usize>,
}

#[async_trait::async_trait]
impl Http for GatedHttp {
    async fn send(&self, _req: HttpRequest) -> Result<HttpResponse, HttpError> {
        *self.posts.lock().unwrap() += 1;
        self.gate.notified().await;
        Ok(HttpResponse::new(200, ""))
    }
    async fn download(&self, _url: &str, _dest: &std::path::Path) -> Result<u64, HttpError> {
        unreachable!()
    }
}

#[tokio::test(start_paused = true)]
async fn flush_waits_for_a_post_the_timer_already_started() {
    // Arrange
    let http = Arc::new(GatedHttp { gate: tokio::sync::Notify::new(), posts: Mutex::new(0) });
    let clock = Arc::new(TestClock::new(UNIX_EPOCH));
    let n = Arc::new(DiscordNotifier::new(hooked(json!({})), "1.0.0", Arc::new(|| 0), http.clone(), clock));
    n.send("batched", Level::Info, None).await;
    pass(Duration::from_secs(5)).await;
    // Act
    let flusher = {
        let n = n.clone();
        tokio::spawn(async move { n.flush().await })
    };
    pass(Duration::from_secs(1)).await;
    let early = flusher.is_finished();
    http.gate.notify_one();
    pass(Duration::from_secs(1)).await;
    // Assert
    assert!(!early);
    assert!(flusher.is_finished());
    assert_eq!(*http.posts.lock().unwrap(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_huge_retry_after_waits_at_most_the_cap() {
    // Arrange
    let http = Arc::new(FakeHttp::new());
    http.route("https://", |_| {
        let mut res = HttpResponse::new(429, "");
        res.headers.push(("retry-after".into(), "3600".into()));
        Ok(res)
    });
    let n = notifier_with(hooked(json!({})), &http);
    let task = {
        let n = n.clone();
        tokio::spawn(async move { n.send("x", Level::Error, None).await })
    };
    // Act
    pass(Duration::from_secs(29)).await;
    let early = task.is_finished();
    pass(Duration::from_secs(2)).await;
    // Assert
    assert!(!early && task.is_finished());
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_beyond_f64_range_of_a_duration_does_not_panic() {
    // Arrange
    let http = Arc::new(FakeHttp::new());
    http.route("https://", |_| {
        let mut res = HttpResponse::new(429, "");
        res.headers.push(("retry-after".into(), "1e300".into()));
        Ok(res)
    });
    let n = notifier_with(hooked(json!({})), &http);
    let task = {
        let n = n.clone();
        tokio::spawn(async move { n.send("x", Level::Error, None).await })
    };
    // Act
    pass(Duration::from_secs(31)).await;
    // Assert
    assert!(task.is_finished());
    assert!(task.await.is_ok());
}
