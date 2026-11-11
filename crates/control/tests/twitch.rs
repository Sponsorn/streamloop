use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use streamloop_common::api::{ControlCmd, EncoderStatus, SupervisorStatus};
use streamloop_common::config::{self, Config};
use streamloop_control::clock::TestClock;
use streamloop_control::discord::{EmbedField, Level, Notifier};
use streamloop_control::events::Alert;
use streamloop_control::http::{FakeHttp, HttpError, HttpResponse};
use streamloop_control::readback::ReadbackState;
use streamloop_control::supervisor_client::fake::{Call, FakeSupervisor};
use streamloop_control::supervisor_client::ControlResult;
use streamloop_control::twitch::{RestartClock, TwitchChecker, TwitchOptions};

const START: Duration = Duration::from_secs(1_792_929_600);
const HALF_HOUR: Duration = Duration::from_secs(30 * 60);

fn config_with(overrides: Value) -> Config {
    let mut root = json!({
        "playlists": [{ "id": "PL123" }],
        "twitchClientId": "test-client-id",
        "twitchClientSecret": "test-client-secret",
        "twitchChannel": "testchannel",
        "twitchLivenessEnabled": true,
    });
    for (k, v) in overrides.as_object().unwrap() {
        root[k] = v.clone();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, root.to_string()).unwrap();
    config::read(&path).unwrap()
}

fn config() -> Config {
    config_with(json!({}))
}

#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Critical(String),
    Mismatch(String),
    Restart(String),
}

#[derive(Default)]
struct RecordingNotifier(Mutex<Vec<Sent>>);

impl RecordingNotifier {
    fn sent(&self) -> Vec<Sent> {
        self.0.lock().unwrap().clone()
    }

    fn criticals(&self) -> Vec<String> {
        self.sent().into_iter().filter_map(|s| if let Sent::Critical(m) = s { Some(m) } else { None }).collect()
    }

    fn count(&self, pick: fn(&Sent) -> bool) -> usize {
        self.sent().iter().filter(|s| pick(s)).count()
    }
}

#[async_trait]
impl Notifier for RecordingNotifier {
    async fn notify_critical(&self, message: &str) {
        self.0.lock().unwrap().push(Sent::Critical(message.to_string()));
    }

    async fn notify_event(&self, _alert: Alert) {}

    async fn notify_twitch_mismatch(&self, channel: &str) {
        self.0.lock().unwrap().push(Sent::Mismatch(channel.to_string()));
    }

    async fn notify_twitch_restart(&self, channel: &str) {
        self.0.lock().unwrap().push(Sent::Restart(channel.to_string()));
    }

    async fn send(&self, _content: &str, _level: Level, _fields: Option<Vec<EmbedField>>) {}

    async fn flush(&self) {}
}

type Answers = Arc<Mutex<VecDeque<Result<HttpResponse, HttpError>>>>;

/// Helix as the checker sees it: queued answers for the token endpoint and for the streams endpoint.
struct Helix {
    http: Arc<FakeHttp>,
    tokens: Answers,
    streams: Answers,
}

fn queue_route(http: &FakeHttp, prefix: &str, answers: &Answers) {
    let answers = answers.clone();
    http.route(prefix, move |_| {
        answers.lock().unwrap().pop_front().unwrap_or_else(|| Err(HttpError::other("nothing queued")))
    });
}

fn json_reply(status: u16, body: Value) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse::new(status, body.to_string()))
}

impl Helix {
    fn new() -> Self {
        let http = Arc::new(FakeHttp::new());
        let (tokens, streams): (Answers, Answers) = Default::default();
        queue_route(&http, "https://id.twitch.tv/oauth2/token", &tokens);
        queue_route(&http, "https://api.twitch.tv/helix/streams", &streams);
        Self { http, tokens, streams }
    }

    fn token(&self, token: &str) -> &Self {
        self.tokens.lock().unwrap().push_back(json_reply(200, json!({ "access_token": token, "expires_in": 3600 })));
        self
    }

    fn streams(&self, reply: Result<HttpResponse, HttpError>) -> &Self {
        self.streams.lock().unwrap().push_back(reply);
        self
    }

    fn offline(&self, checks: usize) -> &Self {
        for _ in 0..checks {
            self.streams(json_reply(200, json!({ "data": [] })));
        }
        self
    }

    fn live(&self) -> &Self {
        self.streams(json_reply(200, json!({ "data": [{ "type": "live" }] })))
    }

    fn requests_to(&self, prefix: &str) -> usize {
        self.http.requests().iter().filter(|r| r.url.starts_with(prefix)).count()
    }
}

struct Rig {
    checker: TwitchChecker,
    supervisor: Arc<FakeSupervisor>,
    notifier: Arc<RecordingNotifier>,
    clock: Arc<TestClock>,
    events: Arc<Mutex<Vec<String>>>,
}

fn status_with_encoder(alive: bool) -> SupervisorStatus {
    SupervisorStatus { state: Some("playing".into()), encoder: Some(EncoderStatus { alive: Some(alive), ..EncoderStatus::default() }), ..SupervisorStatus::default() }
}

fn supervisor(alive: bool) -> Arc<FakeSupervisor> {
    let s = Arc::new(FakeSupervisor::new());
    s.set_status(Some(status_with_encoder(alive)));
    s
}

fn rig_with(config: &Config, helix: &Helix, alive: bool, restart_clock: Option<RestartClock>, mut opts: TwitchOptions) -> Rig {
    let supervisor = supervisor(alive);
    let notifier = Arc::new(RecordingNotifier::default());
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + START));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    opts.add_event = Some(Arc::new(move |m: &str| sink.lock().unwrap().push(m.to_string())));
    opts.restart_clock = restart_clock;
    let checker = TwitchChecker::new(config, supervisor.clone(), notifier.clone(), helix.http.clone(), clock.clone(), opts);
    Rig { checker, supervisor, notifier, clock, events }
}

fn rig(helix: &Helix, alive: bool) -> Rig {
    rig_with(&config(), helix, alive, None, TwitchOptions::default())
}

fn control_calls(rig: &Rig) -> Vec<Call> {
    rig.supervisor.calls().into_iter().filter(|c| matches!(c, Call::Control { .. })).collect()
}


#[test]
fn is_not_enabled_when_the_toggle_is_off() {
    // Arrange
    let r = rig_with(&config_with(json!({ "twitchLivenessEnabled": false })), &Helix::new(), false, None, TwitchOptions::default());
    // Act + Assert
    assert!(!r.checker.enabled());
}

#[test]
fn is_not_enabled_when_the_client_id_is_empty() {
    let r = rig_with(&config_with(json!({ "twitchClientId": "" })), &Helix::new(), false, None, TwitchOptions::default());
    assert!(!r.checker.enabled());
}

#[test]
fn is_not_enabled_when_the_client_secret_is_empty() {
    let r = rig_with(&config_with(json!({ "twitchClientSecret": "" })), &Helix::new(), false, None, TwitchOptions::default());
    assert!(!r.checker.enabled());
}

#[test]
fn is_not_enabled_when_the_channel_is_empty() {
    let r = rig_with(&config_with(json!({ "twitchChannel": "" })), &Helix::new(), false, None, TwitchOptions::default());
    assert!(!r.checker.enabled());
}

#[tokio::test]
async fn needs_no_helix_credentials_with_a_test_hls_url_and_never_asks_twitch() {
    // Arrange
    let helix = Helix::new();
    let cfg = config_with(json!({ "twitchClientId": "", "twitchClientSecret": "" }));
    let r = rig_with(&cfg, &helix, true, None, TwitchOptions { test_hls_url: Some("http://127.0.0.1:8899/index.m3u8".into()), ..TwitchOptions::default() });
    // Act
    r.checker.check().await;
    // Assert
    assert!(r.checker.enabled());
    assert_eq!(r.checker.status().channel_live, Some(true));
    assert!(helix.http.requests().is_empty());
}

#[test]
fn stays_off_without_credentials_when_the_test_hls_url_is_empty() {
    // Arrange
    let cfg = config_with(json!({ "twitchClientId": "", "twitchClientSecret": "" }));
    let r = rig_with(&cfg, &Helix::new(), false, None, TwitchOptions { test_hls_url: Some(String::new()), ..TwitchOptions::default() });
    // Act + Assert
    assert!(!r.checker.enabled());
}

#[test]
fn is_enabled_when_all_credentials_are_present_and_the_toggle_is_on() {
    let r = rig(&Helix::new(), false);
    assert!(r.checker.enabled());
}


#[test]
fn reports_the_initial_status_structure() {
    // Arrange
    let r = rig(&Helix::new(), false);
    // Act
    let status = serde_json::to_value(r.checker.status()).unwrap();
    // Assert
    assert_eq!(
        status,
        json!({
            "enabled": true,
            "channelLive": null,
            "encoderAlive": null,
            "consecutiveMismatches": 0,
            "mismatchThreshold": 3,
            "lastCheckAt": null,
            "lastRestartAt": null,
            "restartCount": 0,
            "readback": { "lastCheckAt": null, "lastDiff": null, "state": "off" },
        })
    );
}

#[test]
fn reports_enabled_false_when_disabled() {
    let r = rig_with(&config_with(json!({ "twitchLivenessEnabled": false })), &Helix::new(), false, None, TwitchOptions::default());
    assert!(!r.checker.status().enabled);
}


#[tokio::test]
async fn start_sets_up_the_timers_and_stop_clears_them() {
    // Arrange
    let r = rig(&Helix::new(), false);
    // Act
    r.checker.start();
    r.checker.stop();
    // Assert
    assert!(r.supervisor.calls().is_empty());
}

#[tokio::test]
async fn start_does_nothing_when_disabled() {
    // Arrange
    let r = rig_with(&config_with(json!({ "twitchLivenessEnabled": false })), &Helix::new(), false, None, TwitchOptions::default());
    // Act
    r.checker.start();
    r.checker.stop();
    // Assert
    assert!(r.supervisor.calls().is_empty());
}

/// Moves time on after the timer task has had a turn to start its sleep, then lets it run.
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
async fn the_first_check_runs_15_s_after_start_and_then_every_poll_interval() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(3);
    let r = rig(&helix, false);
    // Act
    r.checker.start();
    pass(Duration::from_secs(14)).await;
    let before = helix.requests_to("https://api.twitch.tv/");
    pass(Duration::from_secs(1)).await;
    let first = helix.requests_to("https://api.twitch.tv/");
    pass(Duration::from_secs(60)).await;
    let second = helix.requests_to("https://api.twitch.tv/");
    r.checker.stop();
    // Assert
    assert_eq!((before, first, second), (0, 1, 2));
}


#[tokio::test]
async fn counts_a_mismatch_when_the_encoder_is_up_and_twitch_is_offline() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(1);
    let r = rig(&helix, true);
    // Act
    r.checker.check().await;
    // Assert
    assert_eq!(r.checker.status().consecutive_mismatches, 1);
}

#[tokio::test]
async fn resets_the_count_when_both_are_streaming() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(1).live();
    let r = rig(&helix, true);
    r.checker.check().await;
    assert_eq!(r.checker.status().consecutive_mismatches, 1);
    // Act
    r.checker.check().await;
    // Assert
    assert_eq!(r.checker.status().consecutive_mismatches, 0);
}

#[tokio::test]
async fn resets_the_count_when_the_encoder_is_down() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(1);
    let r = rig(&helix, false);
    // Act
    r.checker.check().await;
    // Assert
    assert_eq!(r.checker.status().consecutive_mismatches, 0);
}

#[tokio::test]
async fn restarts_the_encoder_at_the_threshold_of_three_mismatches() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(3);
    let r = rig(&helix, true);
    // Act
    r.checker.check().await;
    assert_eq!(r.checker.status().consecutive_mismatches, 1);
    r.checker.check().await;
    assert_eq!(r.checker.status().consecutive_mismatches, 2);
    assert!(control_calls(&r).is_empty());
    r.checker.check().await;
    // Assert
    assert_eq!(r.checker.status().consecutive_mismatches, 0);
    let calls = control_calls(&r);
    assert_eq!(calls.len(), 1);
    let Call::Control { cmd, index, reason, id } = &calls[0] else { unreachable!() };
    assert_eq!((*cmd, *index, id.as_ref()), (ControlCmd::RestartEncoder, None, None));
    assert_eq!(reason.as_deref(), Some("Twitch shows the channel offline"));
    assert_eq!(r.notifier.count(|s| matches!(s, Sent::Mismatch(c) if c == "testchannel")), 1);
    assert_eq!(r.notifier.count(|s| matches!(s, Sent::Restart(c) if c == "testchannel")), 1);
    assert_eq!(r.checker.status().restart_count, 1);
}

#[tokio::test]
async fn names_a_missing_stream_key_in_plain_words_when_the_encoder_cannot_restart() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(3);
    let r = rig(&helix, true);
    r.supervisor.set_control_result(ControlResult { ok: false, error: Some("needs-stream-key: no encoder runs until a reload brings a working key".into()), already_restarting: false });
    // Act
    for _ in 0..3 {
        r.checker.check().await;
    }
    // Assert
    assert_eq!(r.notifier.count(|s| matches!(s, Sent::Restart(_))), 0);
    assert!(r.notifier.criticals().iter().any(|m| m.contains("no stream key is set")));
    assert_eq!(r.checker.status().restart_count, 0);
}

#[tokio::test]
async fn treats_a_supervisor_that_does_not_answer_as_encoder_down() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(1);
    let r = rig(&helix, true);
    r.supervisor.set_status(None);
    // Act
    r.checker.check().await;
    // Assert
    let status = r.checker.status();
    assert_eq!((status.encoder_alive, status.consecutive_mismatches), (Some(false), 0));
}


#[tokio::test]
async fn leaves_the_count_alone_on_an_api_error() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").streams(Ok(HttpResponse::new(500, "")));
    let r = rig(&helix, true);
    // Act
    r.checker.check().await;
    // Assert
    assert_eq!(r.checker.status().consecutive_mismatches, 0);
    assert_eq!(r.checker.status().channel_live, None);
}

#[tokio::test]
async fn leaves_the_count_alone_when_the_request_fails() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").streams(Err(HttpError::other("network error")));
    let r = rig(&helix, true);
    // Act
    r.checker.check().await;
    // Assert
    assert_eq!(r.checker.status().consecutive_mismatches, 0);
}


#[tokio::test]
async fn fetches_a_token_on_the_first_check() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(1);
    let r = rig(&helix, false);
    // Act
    r.checker.check().await;
    // Assert
    let requests = helix.http.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().any(|q| q.url == "https://id.twitch.tv/oauth2/token" && q.method == "POST"));
}

#[tokio::test]
async fn reuses_the_cached_token_for_later_checks() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(2);
    let r = rig(&helix, false);
    // Act
    r.checker.check().await;
    r.checker.check().await;
    // Assert: the first check fetches the token and the streams, the second only the streams
    assert_eq!(helix.http.requests().len(), 3);
    assert_eq!(helix.requests_to("https://id.twitch.tv/"), 1);
}

#[tokio::test]
async fn asks_for_a_new_token_five_minutes_before_the_old_one_expires() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(1).token("tok456").offline(1);
    let r = rig(&helix, false);
    r.checker.check().await;
    // Act
    r.clock.advance(Duration::from_secs(3600 - 5 * 60 - 1));
    r.checker.check().await;
    let before = helix.requests_to("https://id.twitch.tv/");
    r.clock.advance(Duration::from_secs(2));
    helix.offline(1);
    r.checker.check().await;
    // Assert
    assert_eq!((before, helix.requests_to("https://id.twitch.tv/")), (1, 2));
}

#[tokio::test]
async fn refreshes_the_token_on_a_401_and_retries_once() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").streams(Ok(HttpResponse::new(401, ""))).token("tok456").live();
    let r = rig(&helix, false);
    // Act
    r.checker.check().await;
    // Assert
    assert_eq!(helix.http.requests().len(), 4);
    assert_eq!(r.checker.status().channel_live, Some(true));
    let auth: Vec<String> = helix
        .http
        .requests()
        .iter()
        .filter(|q| q.url.starts_with("https://api.twitch.tv/"))
        .map(|q| q.headers.iter().find(|(k, _)| k == "Authorization").unwrap().1.clone())
        .collect();
    assert_eq!(auth, ["Bearer tok123", "Bearer tok456"]);
}


#[tokio::test]
async fn restarts_at_most_once_per_30_min_and_only_alerts_in_between() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(9);
    let r = rig(&helix, true);
    // Act: a restart, then a second mismatch streak inside 30 min
    for _ in 0..6 {
        r.checker.check().await;
    }
    // Assert
    assert_eq!(control_calls(&r).len(), 1);
    assert_eq!(r.notifier.criticals().len(), 1);
    // Act: a third streak after 30 min
    r.clock.advance(HALF_HOUR);
    for _ in 0..3 {
        r.checker.check().await;
    }
    // Assert
    assert_eq!(control_calls(&r).len(), 2);
}

#[tokio::test]
async fn a_third_streak_inside_the_window_sends_no_discord_call_and_goes_to_the_timeline() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(9);
    let r = rig(&helix, true);
    for _ in 0..6 {
        r.checker.check().await;
    }
    let criticals_after_second = r.notifier.criticals().len();
    // Act
    for _ in 0..3 {
        r.checker.check().await;
    }
    // Assert
    assert_eq!(criticals_after_second, 1);
    assert_eq!(r.notifier.criticals().len(), 1);
    assert_eq!(control_calls(&r).len(), 1);
    assert!(r.events.lock().unwrap().iter().any(|m| m.contains("already restarted")));
}

#[tokio::test]
async fn sends_exactly_one_critical_per_30_min_window_across_helix_and_read_back_triggers() {
    // Arrange
    let helix = Helix::new();
    helix.token("tok123").offline(12);
    let r = rig(&helix, true);
    let discord_calls = || r.notifier.sent().len();

    // Act: window 1 opens with a Helix restart, then four more triggers inside it
    for _ in 0..3 {
        r.checker.check().await; // restart: mismatch + restart notices
    }
    r.checker.handle_frozen().await; // first cap hit: one critical
    for _ in 0..3 {
        r.checker.check().await; // timeline only
    }
    r.checker.handle_frozen().await; // timeline only
    let in_first_window = discord_calls();
    // window 2 opens with a read-back restart, then one Helix streak inside it
    r.clock.advance(HALF_HOUR);
    r.checker.handle_frozen().await; // restart, no Discord notice
    for _ in 0..3 {
        r.checker.check().await; // first cap hit: one critical
    }
    r.checker.handle_frozen().await; // timeline only

    // Assert
    assert_eq!(in_first_window, 3);
    assert_eq!(discord_calls(), 4);
    assert_eq!(r.notifier.criticals().len(), 2);
    assert_eq!(control_calls(&r).len(), 2);
}

#[tokio::test]
async fn a_frozen_read_back_restarts_the_encoder_and_shares_the_30_min_limit() {
    // Arrange
    let r = rig(&Helix::new(), true);
    // Act
    r.checker.handle_frozen().await;
    r.checker.handle_frozen().await;
    // Assert
    let calls = control_calls(&r);
    assert_eq!(calls.len(), 1);
    let Call::Control { cmd, index, reason, id } = &calls[0] else { unreachable!() };
    assert_eq!((*cmd, *index, id.as_ref()), (ControlCmd::RestartEncoder, None, None));
    assert!(reason.as_deref().unwrap().contains("frozen"));
    assert!(r.events.lock().unwrap().iter().any(|m| m.contains("frozen")));
    assert_eq!(r.notifier.criticals().len(), 1);
}

#[tokio::test]
async fn an_unavailable_read_back_only_records_a_timeline_event() {
    // Arrange
    let r = rig(&Helix::new(), true);
    // Act
    r.checker.handle_unavailable("the channel playlist could not be fetched");
    // Assert
    let events = r.events.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("the channel playlist could not be fetched"));
    assert!(control_calls(&r).is_empty());
    assert!(r.notifier.sent().is_empty());
}

#[tokio::test]
async fn keeps_the_30_min_limit_across_a_config_reload() {
    // Arrange: the app passes the same restart clock to every new checker
    let helix = Helix::new();
    let shared = RestartClock::new();
    let first = rig_with(&config(), &helix, true, Some(shared.clone()), TwitchOptions::default());
    first.checker.handle_frozen().await;
    // Act: the cap hit and its critical, then a later hit after another reload
    let second = rig_with(&config(), &helix, true, Some(shared.clone()), TwitchOptions::default());
    second.checker.handle_frozen().await;
    let third = rig_with(&config(), &helix, true, Some(shared.clone()), TwitchOptions::default());
    third.checker.handle_frozen().await;
    // Assert
    let restarts = control_calls(&first).len() + control_calls(&second).len() + control_calls(&third).len();
    let criticals = first.notifier.criticals().len() + second.notifier.criticals().len() + third.notifier.criticals().len();
    assert_eq!((restarts, criticals), (1, 1));
    assert!(shared.last_attempt_at().is_some());
}

#[tokio::test]
async fn a_still_whole_frame_only_records_a_timeline_event() {
    // Arrange
    let r = rig(&Helix::new(), true);
    // Act
    r.checker.handle_still();
    // Assert
    assert_eq!(r.events.lock().unwrap().len(), 1);
    assert!(control_calls(&r).is_empty());
    assert!(r.notifier.sent().is_empty());
}

#[test]
fn runs_no_read_back_without_an_ffmpeg_path() {
    // Arrange
    let r = rig_with(&config(), &Helix::new(), true, None, TwitchOptions { ffmpeg_path: None, ..TwitchOptions::default() });
    // Act + Assert
    assert_eq!(r.checker.status().readback.state, ReadbackState::Off);
}


#[test]
fn runs_a_read_back_when_an_ffmpeg_path_is_given() {
    // Arrange
    let r = rig_with(&config(), &Helix::new(), true, None, TwitchOptions { ffmpeg_path: Some("ffmpeg.exe".into()), ..TwitchOptions::default() });
    // Act + Assert
    assert_eq!(r.checker.status().readback.state, ReadbackState::Ok);
}

#[tokio::test]
async fn a_control_call_from_a_second_checker_inside_the_window_alerts_once_with_the_reason() {
    // Arrange
    let shared = RestartClock::new();
    let helix = Helix::new();
    let r = rig_with(&config(), &helix, true, Some(shared), TwitchOptions::default());
    r.checker.handle_frozen().await;
    // Act
    r.checker.handle_frozen().await;
    // Assert
    assert_eq!(r.notifier.criticals(), ["Twitch shows a frozen picture, and the encoder was already restarted in the last 30 minutes. Check the stream."]);
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

/// One process-wide capture at TRACE, as in discord.rs: a per-test subscriber can be blinded by tracing's call-site cache.
fn captured_logs() -> &'static Capture {
    static LOGS: OnceLock<Capture> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Capture::default();
        let subscriber = tracing_subscriber::fmt().with_writer(logs.clone()).with_ansi(false).with_max_level(tracing::Level::TRACE).finish();
        tracing::subscriber::set_global_default(subscriber).expect("no other test installs a global subscriber");
        logs
    })
}

#[tokio::test]
async fn the_client_secret_and_the_app_token_never_reach_a_log_line() {
    // Arrange
    let logs = captured_logs();
    let (secret, token) = ("SECRET-VALUE-Q7Z", "APP-TOKEN-K3M");
    let helix = Helix::new();
    helix
        .tokens
        .lock()
        .unwrap()
        .extend([json_reply(200, json!({ "access_token": token, "expires_in": 3600 })), Err(HttpError::other(format!("token request failed for {secret}")))]);
    helix.streams(Err(HttpError::other(format!("connection to Helix with {token} and {secret} dropped")))).streams(Ok(HttpResponse::new(401, "")));
    let r = rig_with(&config_with(json!({ "twitchClientSecret": secret })), &helix, true, None, TwitchOptions::default());
    // Act: a failed streams request, then a 401 whose token refresh also fails
    r.checker.check().await;
    r.checker.check().await;
    // Assert
    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("Twitch API request failed") && logged.contains("Twitch token request failed"), "{logged}");
    assert!(!logged.contains(secret) && !logged.contains(token), "a credential reached the log");
    let sent: String = r.notifier.sent().iter().map(|s| format!("{s:?}")).collect();
    assert!(!sent.contains(secret) && !sent.contains(token));
}
