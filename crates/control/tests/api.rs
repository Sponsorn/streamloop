//! The routes of `api.ts`, ported from `api.test.ts`, driven through the router without a socket.

mod support;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{json, Value};
use streamloop_common::api::{ControlCmd, ReloadRequest, SupervisorStatus};
use streamloop_common::paths::Layout;
use streamloop_control::api::{setup_check_with, stream_state, StreamState};
use streamloop_control::launcher::LauncherState;
use streamloop_control::runner::{FakeRunner, RunError, RunErrorKind, RunOutput};
use streamloop_control::supervisor_client::fake::Call;
use streamloop_control::supervisor_client::{ControlResult, ReloadResult};
use streamloop_control::updater::UpdateStatus;
use support::*;

fn playing(extra: Value) -> Value {
    let mut base = json!({ "state": "playing", "reason": null, "encoder": { "alive": true }, "recovery": { "step": "none" }, "hold": null });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn status_of(v: &Value) -> Option<SupervisorStatus> {
    (!v.is_null()).then(|| serde_json::from_value(v.clone()).unwrap())
}

fn controls(rig: &Rig) -> Vec<Call> {
    rig.sup.calls().into_iter().filter(|c| matches!(c, Call::Control { .. })).collect()
}

fn reloads(rig: &Rig) -> Vec<ReloadRequest> {
    rig.sup.calls().into_iter().filter_map(|c| if let Call::Reload(r) = c { Some(r) } else { None }).collect()
}

fn shutdowns(rig: &Rig) -> usize {
    rig.sup.calls().iter().filter(|c| matches!(c, Call::Shutdown)).count()
}

/// The playlist lines yt-dlp prints for `--print "%(id)s\t%(title)s\t%(duration)s"`.
fn listing(videos: &[(&str, f64)]) -> RunOutput {
    RunOutput::ok(&videos.iter().map(|(id, d)| format!("{id}\tTitle {id}\t{d}\n")).collect::<String>())
}

fn fetched_lists(rig: &Rig) -> Vec<String> {
    rig.ytdlp.calls().iter().map(|c| c.args.last().unwrap().rsplit("list=").next().unwrap().to_string()).collect()
}

fn launcher_state(s: &str) -> LauncherState {
    [LauncherState::Missing, LauncherState::Starting, LauncherState::Running, LauncherState::StoppedByUser, LauncherState::Down, LauncherState::Ready]
        .into_iter()
        .find(|l| l.as_str() == s)
        .unwrap_or_else(|| panic!("unknown launcher state {s}"))
}

// Autostart handover.

const MINE: &str = r"C:\Users\Emma Ö\StreamLoop 3";
const OLD: &str = r"D:\old\START.bat";

fn vbs_for(bat: &str) -> String {
    format!("Set WshShell = CreateObject(\"WScript.Shell\")\nWshShell.Run \"\"\"{bat}\"\"\", 1, False")
}

/// The app with the install at `MINE`; the VBS lives in the rig's Startup folder.
fn autostart_app(rig: &Rig) -> axum::Router {
    rig.app_with(|d| d.layout = Layout::new(std::path::Path::new(MINE)))
}

fn vbs_path(rig: &Rig) -> std::path::PathBuf {
    rig.startup_dir().join("StreamLoop.vbs")
}

#[tokio::test]
async fn autostart_writes_a_utf16le_vbs_with_a_bom_that_targets_this_install_start_bat() {
    // Arrange
    let rig = Rig::new();
    let app = autostart_app(&rig);
    // Act
    let r = post(&app, "/autostart", json!({ "enabled": true })).await;
    let g = get(&app, "/autostart").await.json();
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({ "ok": true, "enabled": true }));
    let raw = std::fs::read(vbs_path(&rig)).unwrap();
    assert_eq!(raw[..2], [0xff, 0xfe]);
    let units: Vec<u16> = raw[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text = String::from_utf16(&units).unwrap();
    assert!(text.contains(&format!("WshShell.Run \"\"\"{MINE}\\START.bat\"\"\", 1, False")), "{text}");
    assert!(text.contains("\r\n"), "lines end in CRLF: {text:?}");
    assert_eq!(g, json!({ "enabled": true, "target": format!("{MINE}\\START.bat"), "thisInstall": true }));
}

#[tokio::test]
async fn autostart_reads_a_2x_utf8_vbs_without_a_bom_as_another_install() {
    // Arrange
    let rig = Rig::new();
    let other = r"C:\Users\Emma Ö\StreamLoop 2\START.bat";
    write(&vbs_path(&rig), vbs_for(other).as_bytes());
    // Act
    let g = get(&autostart_app(&rig), "/autostart").await.json();
    // Assert
    assert_eq!(g, json!({ "enabled": true, "target": other, "thisInstall": false }));
}

#[tokio::test]
async fn autostart_reads_true_cp1252_bytes_through_the_ansi_fallback() {
    // Arrange
    let rig = Rig::new();
    let other = "C:UsersEmma ÖStreamLoop 2START.bat";
    let latin1: Vec<u8> = vbs_for(other).chars().map(|c| c as u32 as u8).collect();
    write(&vbs_path(&rig), &latin1);
    // Act
    let g = get(&autostart_app(&rig), "/autostart").await.json();
    // Assert
    assert_eq!(g["target"], other);
}

#[tokio::test]
async fn autostart_leaves_no_temp_file_behind_after_enabling() {
    // Arrange
    let rig = Rig::new();
    // Act
    post(&autostart_app(&rig), "/autostart", json!({ "enabled": true })).await;
    // Assert
    assert!(!rig.startup_dir().join("StreamLoop.vbs.tmp").exists());
    assert!(vbs_path(&rig).exists());
}

#[tokio::test]
async fn autostart_disabling_a_vbs_without_a_readable_target_answers_409_with_its_own_reason() {
    // Arrange
    let rig = Rig::new();
    write(&vbs_path(&rig), b"MsgBox \"hi\"");
    // Act
    let r = post(&autostart_app(&rig), "/autostart", json!({ "enabled": false })).await;
    // Assert
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.error(), "StreamLoop.vbs in the Startup folder was not written by StreamLoop. Remove it from the Startup folder by hand.");
    assert!(vbs_path(&rig).exists());
}

#[tokio::test]
async fn autostart_answers_500_json_when_the_vbs_cannot_be_read() {
    // Arrange
    let rig = Rig::new();
    std::fs::create_dir_all(vbs_path(&rig)).unwrap();
    // Act
    let r = get(&autostart_app(&rig), "/autostart").await;
    // Assert
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(r.content_type, "application/json; charset=utf-8");
    assert!(!r.error().is_empty());
}

#[tokio::test]
async fn autostart_reads_a_utf8_vbs_with_a_bom_and_ignores_path_case() {
    // Arrange
    let rig = Rig::new();
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(vbs_for(&format!("{}\\start.bat", MINE.to_lowercase())).as_bytes());
    write(&vbs_path(&rig), &bytes);
    // Act
    let g = get(&autostart_app(&rig), "/autostart").await.json();
    // Assert
    assert_eq!(g["thisInstall"], true);
}

#[tokio::test]
async fn autostart_reports_disabled_when_there_is_no_vbs() {
    // Arrange
    let rig = Rig::new();
    // Act
    let g = get(&autostart_app(&rig), "/autostart").await.json();
    // Assert
    assert_eq!(g, json!({ "enabled": false, "target": null, "thisInstall": false }));
}

#[tokio::test]
async fn autostart_enabling_replaces_another_install_vbs() {
    // Arrange
    let rig = Rig::new();
    let app = autostart_app(&rig);
    write(&vbs_path(&rig), vbs_for(OLD).as_bytes());
    // Act
    post(&app, "/autostart", json!({ "enabled": true })).await;
    let g = get(&app, "/autostart").await.json();
    // Assert
    assert_eq!(g["thisInstall"], true);
}

#[tokio::test]
async fn autostart_disabling_removes_this_install_vbs() {
    // Arrange
    let rig = Rig::new();
    let app = autostart_app(&rig);
    post(&app, "/autostart", json!({ "enabled": true })).await;
    // Act
    let r = post(&app, "/autostart", json!({ "enabled": false })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({ "ok": true, "enabled": false }));
    assert!(!vbs_path(&rig).exists());
}

#[tokio::test]
async fn autostart_disabling_leaves_another_install_vbs_and_answers_409() {
    // Arrange
    let rig = Rig::new();
    write(&vbs_path(&rig), vbs_for(OLD).as_bytes());
    // Act
    let r = post(&autostart_app(&rig), "/autostart", json!({ "enabled": false })).await;
    // Assert
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.error(), format!("Autostart points to another StreamLoop install: {OLD}."));
    assert!(vbs_path(&rig).exists());
}

// GET /status stream.state.

#[tokio::test]
async fn status_reports_the_stream_state_for_each_supervisor_and_launcher_state() {
    let rows: Vec<(&str, Value, LauncherState, &str)> = vec![
        ("live while the encoder is up and nothing recovers", playing(json!({})), LauncherState::Running, "live"),
        ("recovering during a retry", playing(json!({ "state": "recovering", "recovery": { "step": "retry" } })), LauncherState::Running, "recovering"),
        ("recovering while the encoder restarts", playing(json!({ "state": "recovering", "encoder": { "alive": false } })), LauncherState::Running, "recovering"),
        ("recovering during a hold on the slate", playing(json!({ "hold": { "reason": "every video failed", "level": 1 } })), LauncherState::Running, "recovering"),
        ("starting while the supervisor starts", playing(json!({ "state": "starting", "encoder": { "alive": false } })), LauncherState::Running, "starting"),
        ("off-air without a stream key", playing(json!({ "state": "needs-stream-key", "encoder": { "alive": false } })), LauncherState::Running, "off-air"),
        ("off-air without a playlist", playing(json!({ "state": "waiting-for-playlist", "encoder": { "alive": false } })), LauncherState::Running, "off-air"),
        ("off-air while the supervisor stops on its own", playing(json!({ "state": "stopping" })), LauncherState::Running, "off-air"),
        ("off-air when playing with the encoder down", playing(json!({ "encoder": { "alive": false } })), LauncherState::Running, "off-air"),
        ("off-air when the supervisor does not answer", Value::Null, LauncherState::Down, "off-air"),
        ("off-air when the supervisor is missing", Value::Null, LauncherState::Missing, "off-air"),
        ("starting while the launcher starts it", Value::Null, LauncherState::Starting, "starting"),
    ];
    for (name, sup, launcher, expected) in rows {
        // Arrange
        let rig = Rig::new();
        rig.sup.set_status(status_of(&sup));
        rig.launcher.set_state(launcher);
        // Act
        let body = get(&rig.app(), "/status").await.json();
        // Assert
        assert_eq!(body["stream"]["state"], expected, "{name}");
        if expected == "live" {
            assert!(body["stream"]["reason"].is_null(), "{name}");
        } else {
            assert!(body["stream"]["reason"].is_string(), "{name}");
        }
        assert_eq!(body["launcher"], launcher.as_str(), "{name}");
        assert_eq!(body["supervisor"], sup, "{name}");
    }
}

#[tokio::test]
async fn status_is_stopped_after_stop_stream_even_while_the_old_supervisor_still_answers() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({}))));
    rig.launcher.set_stopped(true);
    // Act
    let body = get(&rig.app(), "/status").await.json();
    // Assert
    assert_eq!(body["stream"]["state"], "stopped");
    let reason = body["stream"]["reason"].as_str().unwrap();
    assert!(reason.contains("stopped from the dashboard"), "{reason}");
    assert!(reason.contains("until you press Start"), "{reason}");
}

#[tokio::test]
async fn status_shows_ready_to_stream_while_the_launcher_waits_for_start() {
    // Arrange
    let rig = Rig::new();
    rig.launcher.set_state(LauncherState::Ready);
    // Act
    let body = get(&rig.app(), "/status").await.json();
    // Assert
    assert_eq!(body["stream"], json!({ "state": "stopped", "reason": "Ready to stream. Press Start." }));
}

#[tokio::test]
async fn status_is_starting_with_the_launcher_reason_while_a_start_waits_for_the_stopped_supervisor() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({ "state": "stopping" }))));
    rig.launcher.set_state(LauncherState::Starting);
    rig.launcher.set_wait_reason(Some("Waiting for the stopped supervisor to exit before starting it again."));
    // Act
    let body = get(&rig.app(), "/status").await.json();
    // Assert
    assert_eq!(body["stream"], json!({ "state": "starting", "reason": "Waiting for the stopped supervisor to exit before starting it again." }));
}

#[tokio::test]
async fn status_is_off_air_with_the_launcher_reason_while_the_launch_is_held() {
    // Arrange
    let rig = Rig::new();
    rig.launcher.set_state(LauncherState::Starting);
    rig.launcher.set_block_reason(Some("OBS is running."));
    // Act
    let body = get(&rig.app(), "/status").await.json();
    // Assert
    assert_eq!(body["stream"], json!({ "state": "off-air", "reason": "OBS is running." }));
}

#[tokio::test]
async fn status_keeps_first_run_system_memory_uptime_ms_and_twitch() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = get(&rig.app(), "/status").await;
    let body = r.json();
    // Assert
    assert_eq!(r.content_type, "application/json; charset=utf-8");
    assert_eq!(body["firstRun"], false);
    assert_eq!(body["uptimeMs"], 1234);
    assert!(body["systemMemory"]["totalGB"].as_f64().unwrap() > 0.0);
    for key in ["freeGB", "usedGB", "usedPercent"] {
        assert!(body["systemMemory"][key].is_number(), "{key}");
    }
    assert_eq!(body["twitch"]["enabled"], false);
    assert_eq!(body["twitch"]["readback"]["state"], "off");
}

#[test]
fn matches_every_stream_state_fixture() {
    // Arrange
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stream-state.json")).unwrap();
    let cases: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(cases.len(), 42);
    for case in cases {
        let input = &case["input"];
        let sup = status_of(&input["sup"]);
        let launcher = launcher_state(input["launcher"].as_str().unwrap());
        // Act
        let (state, reason) = stream_state(
            sup.as_ref(),
            launcher,
            input["stoppedByUser"].as_bool().unwrap(),
            input["waitReason"].as_str(),
            input["blockReason"].as_str(),
        );
        // Assert
        let want = &case["output"];
        assert_eq!(state.as_str(), want["state"].as_str().unwrap(), "{}", case["name"]);
        assert_eq!(reason.as_deref(), want["reason"].as_str(), "{}", case["name"]);
    }
}

#[test]
fn stream_state_serialises_as_the_dashboard_reads_it() {
    // Act
    let names: Vec<String> = [StreamState::Live, StreamState::Recovering, StreamState::OffAir, StreamState::Starting, StreamState::Stopped]
        .iter()
        .map(|s| serde_json::to_string(s).unwrap())
        .collect();
    // Assert
    assert_eq!(names, ["\"live\"", "\"recovering\"", "\"off-air\"", "\"starting\"", "\"stopped\""]);
}

// GET /state and /events.

#[tokio::test]
async fn state_returns_now_playing_from_the_supervisor() {
    // Arrange
    let rig = Rig::new();
    let p = json!({ "index": 3, "id": "abc", "title": "T", "position": 12.5, "duration": 600, "source": "file" });
    rig.sup.set_status(status_of(&playing(json!({ "playing": p }))));
    // Act
    let body = get(&rig.app(), "/state").await.json();
    // Assert
    assert_eq!(body, json!({ "index": 3, "id": "abc", "title": "T", "position": 12.5, "duration": 600.0, "source": "file" }));
}

#[tokio::test]
async fn state_returns_nulls_when_the_supervisor_is_unreachable() {
    // Arrange
    let rig = Rig::new();
    // Act
    let body = get(&rig.app(), "/state").await.json();
    // Assert
    assert_eq!(body, json!({ "index": null, "id": null, "title": null, "position": null, "duration": null, "source": null }));
}

#[tokio::test]
async fn events_returns_the_timeline() {
    // Arrange
    let rig = Rig::new();
    rig.timeline.add("m");
    // Act
    let body = get(&rig.app(), "/events").await.json();
    // Assert
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["message"], "m");
    assert!(entries[0]["timestamp"].is_string());
}

// POST /stream-key.

#[tokio::test]
async fn stream_key_stores_only_the_blob_and_never_echoes_or_logs_the_key() {
    // Arrange
    let rig = Rig::new();
    let logs = captured_logs();
    // Act
    let r = post(&rig.app(), "/stream-key", json!({ "key": KEY })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({ "ok": true, "streamKeySet": true }));
    assert!(!r.body.contains(KEY));
    assert_eq!(*rig.protected.lock().unwrap(), [KEY]);
    assert_eq!(rig.saved()["supervisor"]["streamKeyProtected"], BLOB);
    assert!(!rig.config_text().contains(KEY));
    assert_eq!(reloads(&rig).len(), 1);
    assert!(!logs.text().contains(KEY));
}

#[tokio::test]
async fn stream_key_keeps_the_key_out_of_the_error_when_protect_fails() {
    // Arrange
    let rig = Rig::new();
    let logs = captured_logs();
    let before = rig.config_text();
    let app = rig.app_with(|d| d.protect = Arc::new(|key: &str| Err(format!("protect failed: bad input {key}"))));
    // Act
    let r = post(&app, "/stream-key", json!({ "key": KEY })).await;
    // Assert
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(r.error(), "protect failed: bad input [redacted]");
    assert_eq!(rig.config_text(), before);
    assert!(reloads(&rig).is_empty());
    assert!(!logs.text().contains(KEY));
}

#[tokio::test]
async fn stream_key_answers_malformed_json_without_echoing_or_logging_the_key() {
    // Arrange
    let rig = Rig::new();
    let logs = captured_logs();
    // Act
    let r = post_raw(&rig.app(), "/stream-key", &format!("{{\"key\":{KEY}}}")).await;
    // Assert
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json(), json!({ "error": "Invalid JSON" }));
    assert!(!r.body.contains("TESTKEY"));
    assert!(!logs.text().contains("TESTKEY"));
    assert!(rig.protected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stream_key_rejects_an_empty_or_missing_key() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    // Act
    let blank = post(&app, "/stream-key", json!({ "key": "  " })).await;
    let missing = post(&app, "/stream-key", json!({})).await;
    // Assert
    for r in [blank, missing] {
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "No stream key given".to_string()));
    }
    assert!(rig.protected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stream_key_is_protected_in_process() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app_with(|d| d.protect = Arc::new(streamloop_common::secret::protect));
    // Act
    let r = post(&app, "/stream-key", json!({ "key": format!("  {KEY}  ") })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let blob = rig.saved()["supervisor"]["streamKeyProtected"].as_str().unwrap().to_string();
    assert_ne!(blob, BLOB);
    assert_eq!(streamloop_common::secret::unprotect(&blob).unwrap(), KEY);
    assert!(!rig.config_text().contains(KEY));
}

#[tokio::test]
async fn stream_key_never_reaches_a_log_line() {
    // Arrange
    let rig = Rig::new();
    let logs = captured_logs();
    let app = rig.app_with(|d| d.protect = Arc::new(streamloop_common::secret::protect));
    rig.sup.set_reload_result(ReloadResult { ok: false, restart_required: false, error: Some("fetch failed".into()), code: Some("ECONNREFUSED".into()) });
    // Act
    let r = post(&app, "/stream-key", json!({ "key": KEY })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let logged = logs.text();
    assert!(logged.contains("Stream key replaced"), "the capture saw the route's own line: {logged}");
    assert!(!logged.contains(KEY), "{logged}");
}

// Config.

#[tokio::test]
async fn config_refuses_a_cookie_browser_value_that_is_too_long_or_has_a_control_character_and_saves_nothing() {
    for (why, value) in [("longer than 200 characters", "x".repeat(201)), ("a control character", "fire\nfox".to_string())] {
        // Arrange
        let rig = Rig::new();
        let before = rig.config_text();
        // Act
        let r = post(&rig.app(), "/config", json!({ "ytdlCookiesFromBrowser": value })).await;
        // Assert
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{why}");
        assert!(r.error().contains(why), "{why}: {}", r.error());
        assert_eq!(rig.config_text(), before, "{why}");
        assert!(reloads(&rig).is_empty(), "{why}");
    }
}

#[tokio::test]
async fn config_cookie_texts_match_node() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    // Act
    let long = post(&app, "/config", json!({ "ytdlCookiesFromBrowser": "x".repeat(201) })).await;
    let control = post(&app, "/config", json!({ "ytdlCookiesFromBrowser": "firefox\u{7}" })).await;
    let not_text = post(&app, "/config", json!({ "ytdlCookiesFromBrowser": 7 })).await;
    let padded = post(&app, "/config", json!({ "ytdlCookiesFromBrowser": format!("  {}  ", "x".repeat(200)) })).await;
    // Assert
    assert_eq!(long.error(), "\"yt-dlp cookies from browser\" is longer than 200 characters.");
    assert_eq!(control.error(), "\"yt-dlp cookies from browser\" contains a control character.");
    assert_eq!(not_text.error(), "\"yt-dlp cookies from browser\" must be text.");
    assert_eq!(padded.status, StatusCode::OK, "{}", padded.body);
}

#[tokio::test]
async fn config_reports_whether_a_key_is_set_and_never_the_blob() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = get(&rig.app(), "/config").await;
    let body = r.json();
    // Assert
    assert!(!r.body.contains(BLOB));
    assert_eq!(body["supervisor"]["streamKeySet"], true);
    assert!(body["supervisor"].get("streamKeyProtected").is_none());
    assert_eq!(body["twitchClientSecret"], "********");
    assert_eq!(body["discord"]["webhookUrl"], "********");
    assert_eq!(body["autoStartStream"], true);
}

#[tokio::test]
async fn config_masks_an_empty_secret_as_empty() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    post(&app, "/config", json!({ "twitchClientSecret": "", "discord": { "webhookUrl": "" } })).await;
    // Act
    let body = get(&app, "/config").await.json();
    // Assert
    assert_eq!((body["twitchClientSecret"].as_str(), body["discord"]["webhookUrl"].as_str()), (Some(""), Some("")));
}

#[tokio::test]
async fn config_ignores_a_stream_key_in_the_body_applies_and_reloads() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_reload_result(ReloadResult { ok: true, restart_required: true, error: None, code: None });
    // Act
    let r = post(&rig.app(), "/config", json!({ "supervisor": { "encoder": "h264_amf", "streamKeyProtected": "other", "streamKeySet": false } })).await;
    let body = r.json();
    // Assert
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let saved = rig.saved()["supervisor"].clone();
    assert_eq!((saved["encoder"].as_str(), saved["preferredEncoder"].as_str()), (Some("h264_amf"), Some("h264_amf")));
    assert_eq!(saved["streamKeyProtected"], BLOB);
    assert!(saved.get("streamKeySet").is_none());
    assert_eq!(rig.applied.load(Ordering::SeqCst), 1);
    assert_eq!(reloads(&rig), [ReloadRequest::default()]);
    assert_eq!((body["ok"].as_bool(), body["restartRequired"].as_bool()), (Some(true), Some(true)));
    assert!(body["reloadError"].is_null());
    assert!(!r.body.contains(BLOB));
    assert_eq!(body["config"]["supervisor"]["encoder"], "h264_amf");
}

#[tokio::test]
async fn config_keeps_masked_secrets_when_the_dashboard_sends_them_back() {
    // Arrange
    let rig = Rig::new();
    // Act
    post(&rig.app(), "/config", json!({ "twitchClientSecret": "********", "discord": { "webhookUrl": "********" }, "twitchChannel": "other" })).await;
    // Assert
    let saved = rig.saved();
    assert_eq!((saved["twitchClientSecret"].as_str(), saved["discord"]["webhookUrl"].as_str()), (Some("secret"), Some("https://discord.example/hook")));
    assert_eq!(saved["twitchChannel"], "other");
}

#[tokio::test]
async fn config_reports_a_failed_reload() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_reload_result(ReloadResult { ok: false, restart_required: false, error: Some("fetch failed".into()), code: Some("ECONNREFUSED".into()) });
    // Act
    let body = post(&rig.app(), "/config", json!({ "twitchChannel": "x" })).await.json();
    // Assert
    assert_eq!(body["ok"], true);
    assert_eq!(body["restartRequired"], false);
    assert_eq!(body["reloadError"], "fetch failed");
    assert_eq!(body["reloadErrorCode"], "ECONNREFUSED");
}

#[tokio::test]
async fn config_gives_a_null_reload_error_code_when_the_supervisor_answered_with_an_error() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_reload_result(ReloadResult { ok: false, restart_required: false, error: Some("bad config".into()), code: None });
    // Act
    let body = post(&rig.app(), "/config", json!({ "twitchChannel": "x" })).await.json();
    // Assert
    assert_eq!(body["reloadError"], "bad config");
    assert!(body["reloadErrorCode"].is_null());
}

#[tokio::test]
async fn config_answers_400_naming_each_refused_field_and_saves_nothing() {
    // Arrange
    let rig = Rig::new();
    let before = rig.config_text();
    // Act
    let r = post(&rig.app(), "/config", json!({ "playlists": [], "supervisor": { "cacheBudgetMb": 10 } })).await;
    // Assert
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let error = r.error();
    let fields: Vec<&str> = error.split("; ").map(|part| part.split(": ").next().unwrap()).collect();
    assert_eq!(fields, ["playlists", "supervisor.cacheBudgetMb"], "{error}");
    assert_eq!(rig.config_text(), before);
    assert_eq!(rig.applied.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn config_that_changes_the_encoder_also_sets_preferred_encoder() {
    // Arrange
    let rig = Rig::new();
    // Act
    post(&rig.app(), "/config", json!({ "supervisor": { "encoder": "h264_amf" } })).await;
    // Assert
    let saved = rig.saved()["supervisor"].clone();
    assert_eq!((saved["encoder"].as_str(), saved["preferredEncoder"].as_str()), (Some("h264_amf"), Some("h264_amf")));
}

#[tokio::test]
async fn config_takes_an_explicit_preferred_encoder_even_for_the_encoder_running_now() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    post(&app, "/config", json!({ "supervisor": { "encoder": "libx264", "preferredEncoder": "h264_nvenc" } })).await;
    // Act
    post(&app, "/config", json!({ "supervisor": { "encoder": "libx264", "preferredEncoder": "libx264" } })).await;
    // Assert
    let saved = rig.saved()["supervisor"].clone();
    assert_eq!((saved["encoder"].as_str(), saved["preferredEncoder"].as_str()), (Some("libx264"), Some("libx264")));
}

#[tokio::test]
async fn config_keeps_an_explicit_preferred_encoder_over_the_changed_encoder_rule() {
    // Arrange
    let rig = Rig::new();
    // Act
    post(&rig.app(), "/config", json!({ "supervisor": { "encoder": "h264_amf", "preferredEncoder": "h264_qsv" } })).await;
    // Assert
    let saved = rig.saved()["supervisor"].clone();
    assert_eq!((saved["encoder"].as_str(), saved["preferredEncoder"].as_str()), (Some("h264_amf"), Some("h264_qsv")));
}

#[tokio::test]
async fn config_that_resends_the_current_encoder_leaves_the_preference_alone() {
    // Arrange
    let rig = Rig::new();
    // Act
    post(&rig.app(), "/config", json!({ "supervisor": { "encoder": "h264_nvenc", "overlay": false } })).await;
    // Assert
    let saved = rig.saved()["supervisor"].clone();
    assert_eq!((saved["encoder"].as_str(), saved["overlay"].as_bool()), (Some("h264_nvenc"), Some(false)));
    assert!(saved.get("preferredEncoder").is_none(), "{saved}");
}

#[tokio::test]
async fn config_save_is_what_later_reads_see() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    // Act
    post(&app, "/config", json!({ "playlists": [{ "id": "PLwizardPlaylist01" }] })).await;
    let body = get(&app, "/config").await.json();
    // Assert
    assert_eq!(body["playlists"], json!([{ "id": "PLwizardPlaylist01" }]));
    assert_eq!(rig.config.read().unwrap().playlists[0].id, "PLwizardPlaylist01");
}

// Logs.

fn set_mtime(path: &std::path::Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
}

fn logs_rig() -> Rig {
    let rig = Rig::new();
    let sup = rig.layout.data.join("logs");
    let node = rig.layout.app_logs.clone();
    write(&sup.join("events-2026-10-25.jsonl"), b"{\"kind\":\"x\"}\n");
    write(&node.join("streamloop.log"), b"node line\n");
    write(&sup.join("secret.txt"), b"nope");
    set_mtime(&sup.join("events-2026-10-25.jsonl"), 2000);
    set_mtime(&node.join("streamloop.log"), 3000);
    rig
}

#[tokio::test]
async fn logs_lists_both_sources_newest_first() {
    // Arrange
    let rig = logs_rig();
    // Act
    let body = get(&rig.app(), "/logs").await.json();
    // Assert
    let names: Vec<(&str, &str)> = body.as_array().unwrap().iter().map(|f| (f["name"].as_str().unwrap(), f["source"].as_str().unwrap())).collect();
    assert_eq!(names, [("streamloop.log", "node"), ("events-2026-10-25.jsonl", "supervisor")]);
    assert_eq!(body[0]["size"], 10);
    assert_eq!(body[0]["mtime"], "1970-01-01T00:50:00.000Z");
}

#[tokio::test]
async fn logs_source_name_returns_the_file() {
    // Arrange
    let rig = logs_rig();
    // Act
    let r = get(&rig.app(), "/logs/supervisor/events-2026-10-25.jsonl").await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "text/plain; charset=utf-8");
    assert_eq!(r.body, "{\"kind\":\"x\"}\n");
}

#[tokio::test]
async fn logs_answers_404_for_a_folder_whose_name_passes_the_check() {
    // Arrange
    let rig = logs_rig();
    std::fs::create_dir_all(rig.layout.app_logs.join("con.log")).unwrap();
    // Act
    let r = get(&rig.app(), "/logs/node/con.log").await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::NOT_FOUND, "Log file not found".to_string()));
}

#[tokio::test]
async fn logs_answers_404_for_a_missing_file() {
    // Arrange
    let rig = logs_rig();
    // Act
    let r = get(&rig.app(), "/logs/node/missing.log").await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::NOT_FOUND, "Log file not found".to_string()));
}

#[tokio::test]
async fn logs_reject_traversal_other_extensions_and_unknown_sources() {
    // Arrange
    let rig = logs_rig();
    let app = rig.app();
    let rows = [
        ("a backslash traversal", "/logs/supervisor/..%5Cx.log", "Invalid log file name"),
        ("a slash traversal", "/logs/supervisor/..%2F..%2Fx.log", "Invalid log file name"),
        ("an absolute path", "/logs/node/C%3A%5Cx.log", "Invalid log file name"),
        ("another extension", "/logs/supervisor/secret.txt", "Invalid log file name"),
        ("an unknown source", "/logs/other/streamloop.log", "Unknown log source"),
    ];
    for (name, path, error) in rows {
        // Act
        let r = get(&app, path).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, error.to_string()), "{name}");
    }
}

#[tokio::test]
async fn logs_show_the_last_megabyte_of_a_bigger_file() {
    // Arrange
    let rig = Rig::new();
    let mut text = "a".repeat(10);
    text.push_str(&"b".repeat(1024 * 1024));
    write(&rig.layout.app_logs.join("big.log"), text.as_bytes());
    // Act
    let r = get(&rig.app(), "/logs/node/big.log").await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, format!("... (truncated, showing last 1 MB) ...\n{}", "b".repeat(1024 * 1024)));
}

// Player controls.

#[tokio::test]
async fn player_next_prev_and_restart_return_503_while_the_supervisor_is_down() {
    for cmd in ["next", "prev", "restart"] {
        // Arrange
        let rig = Rig::new();
        rig.launcher.set_state(LauncherState::Down);
        // Act
        let r = post(&rig.app(), &format!("/player/{cmd}"), json!({})).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, "The supervisor is not running.".to_string()), "{cmd}");
        assert!(controls(&rig).is_empty(), "{cmd}");
        assert!(rig.setup_runs.lock().unwrap().is_empty(), "{cmd}");
    }
}

#[tokio::test]
async fn player_jump_returns_503_after_stop_stream() {
    // Arrange
    let rig = Rig::new();
    rig.launcher.set_stopped(true);
    // Act
    let r = post(&rig.app(), "/player/jump", json!({ "index": 2 })).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, "The stream is stopped. Start it first.".to_string()));
}

#[tokio::test]
async fn player_controls_send_their_command() {
    for (cmd, want) in [("next", ControlCmd::Next), ("prev", ControlCmd::Prev), ("restart", ControlCmd::Restart)] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), &format!("/player/{cmd}"), json!({})).await;
        // Assert
        assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true })), "{cmd}");
        assert_eq!(controls(&rig), [Call::Control { cmd: want, index: None, reason: None, id: None }], "{cmd}");
    }
}

#[tokio::test]
async fn player_jump_sends_the_index() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/player/jump", json!({ "index": 2 })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(controls(&rig), [Call::Control { cmd: ControlCmd::Jump, index: Some(2), reason: None, id: None }]);
}

#[tokio::test]
async fn player_jump_sends_the_video_id_with_the_index() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/player/jump", json!({ "index": 2, "id": "dQw4w9WgXc_-" })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(controls(&rig), [Call::Control { cmd: ControlCmd::Jump, index: Some(2), reason: None, id: Some("dQw4w9WgXc_-".into()) }]);
}

#[tokio::test]
async fn player_jump_rejects_an_index_that_is_not_a_video_number() {
    for index in [json!(1_000_000), json!(-1), json!(1.5), json!("3"), Value::Null] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), "/player/jump", json!({ "index": index })).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid video index".to_string()), "{index}");
        assert!(controls(&rig).is_empty());
    }
}

#[tokio::test]
async fn player_jump_rejects_a_bad_id() {
    for id in [json!(""), json!("a b"), json!("x".repeat(65)), json!(7)] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), "/player/jump", json!({ "index": 1, "id": id })).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid video id".to_string()), "{id}");
        assert!(controls(&rig).is_empty());
    }
}

#[tokio::test]
async fn player_jump_without_a_json_body_has_no_index() {
    // Arrange
    let rig = Rig::new();
    let req = Request::builder().method(Method::POST).uri("/api/player/jump").header("x-api-token", TOKEN).body(Body::from("{\"index\":3}")).unwrap();
    // Act
    let r = call(&rig.app(), req).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid video index".to_string()));
}

#[tokio::test]
async fn player_next_passes_on_a_refusal_as_a_reason() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_control_result(ControlResult { ok: false, error: Some("stopping".into()), already_restarting: false });
    // Act
    let r = post(&rig.app(), "/player/next", json!({})).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::BAD_GATEWAY, "The supervisor refused: stopping".to_string()));
}

#[tokio::test]
async fn player_refusal_without_a_reason_says_unknown_error() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_control_result(ControlResult { ok: false, error: None, already_restarting: false });
    // Act
    let r = post(&rig.app(), "/player/prev", json!({})).await;
    // Assert
    assert_eq!(r.error(), "The supervisor refused: unknown error");
}

// Stream stop and start.

#[tokio::test]
async fn stream_stop_marks_the_user_stop_and_shuts_the_supervisor_down() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/stream/stop", json!({})).await;
    // Assert
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(rig.launcher.marks(), 1);
    assert_eq!(shutdowns(&rig), 1);
}

#[tokio::test]
async fn stream_start_launches_now() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/stream/start", json!({})).await;
    // Assert
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(rig.launcher.launches(), 1);
}

// Playlists.

fn on_air(rig: &Rig, playlist_id: Value) {
    rig.sup.set_status(status_of(&playing(json!({ "playing": { "index": 0, "playlistId": playlist_id } }))));
}

#[tokio::test]
async fn playlist_switch_asks_the_supervisor_for_that_list() {
    // Arrange
    let rig = Rig::new();
    on_air(&rig, json!("PLaaa"));
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 2 })).await;
    // Assert
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(reloads(&rig), [ReloadRequest { playlist: Some("PLccc".into()), index: None, id: None }]);
}

#[tokio::test]
async fn playlist_switch_leaves_the_configured_order_alone() {
    // Arrange
    let rig = Rig::new();
    on_air(&rig, json!("PLaaa"));
    let before = rig.config_text();
    // Act
    post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 2 })).await;
    // Assert
    assert_eq!(rig.config_text(), before);
    assert_eq!(rig.applied.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn playlist_switch_rejects_a_bad_index() {
    for index in [json!(3), json!(-1), json!("0"), json!(1.5), Value::Null] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": index })).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid playlist index".to_string()), "{index}");
        assert!(reloads(&rig).is_empty());
    }
}

#[tokio::test]
async fn playlist_switch_passes_the_start_video_to_the_supervisor() {
    // Arrange
    let rig = Rig::new();
    on_air(&rig, json!("PLaaa"));
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1, "videoIndex": 4, "videoId": "abc_DEF-123" })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(reloads(&rig), [ReloadRequest { playlist: Some("PLbbb".into()), index: Some(4), id: Some("abc_DEF-123".into()) }]);
}

#[tokio::test]
async fn playlist_switch_works_before_the_supervisor_has_a_list_on_air() {
    // Arrange
    let rig = Rig::new();
    on_air(&rig, Value::Null);
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1 })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(reloads(&rig), [ReloadRequest { playlist: Some("PLbbb".into()), index: None, id: None }]);
}

#[tokio::test]
async fn playlist_switch_answers_503_while_the_stream_is_stopped() {
    // Arrange
    let rig = Rig::new();
    rig.launcher.set_stopped(true);
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1 })).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::SERVICE_UNAVAILABLE, "The stream is stopped. Start it first.".to_string()));
    assert!(reloads(&rig).is_empty());
}

#[tokio::test]
async fn playlist_switch_answers_503_while_the_supervisor_is_not_running() {
    // Arrange
    let rig = Rig::new();
    rig.launcher.set_state(LauncherState::Down);
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1 })).await;
    // Assert
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(reloads(&rig).is_empty());
}

#[tokio::test]
async fn playlist_switch_reports_a_refused_switch() {
    // Arrange
    let rig = Rig::new();
    on_air(&rig, json!("PLaaa"));
    rig.sup.set_reload_result(ReloadResult { ok: false, restart_required: false, error: Some("playlist PLbbb is not in config.json".into()), code: None });
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1 })).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::BAD_GATEWAY, "The supervisor refused: playlist PLbbb is not in config.json".to_string()));
}

#[tokio::test]
async fn playlist_switch_says_the_full_zip_is_needed_when_the_supervisor_names_no_list() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({ "playing": { "index": 0 } }))));
    // Act
    let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1 })).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::CONFLICT, "Switching playlists needs a newer supervisor. Install the full ZIP.".to_string()));
    assert!(reloads(&rig).is_empty());
}

#[tokio::test]
async fn playlist_switch_rejects_a_bad_video_id() {
    for id in [json!(""), json!("a/b"), json!("x".repeat(65)), json!(7), Value::Null] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1, "videoIndex": 0, "videoId": id })).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid video id".to_string()), "{id}");
        assert!(reloads(&rig).is_empty());
    }
}

#[tokio::test]
async fn playlist_switch_rejects_a_bad_video_index() {
    for index in [json!(-1), json!(1.5), json!("2"), Value::Null, json!(1_000_000)] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), "/playlist/switch", json!({ "playlistIndex": 1, "videoIndex": index })).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid video index".to_string()), "{index}");
        assert!(reloads(&rig).is_empty());
    }
}

#[tokio::test]
async fn playlist_videos_browses_another_playlist_through_the_cache_with_no_current_video() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({ "playing": { "index": 1 } }))));
    rig.ytdlp.push(Ok(listing(&[("v1", 100.0), ("v2", 200.0)])));
    // Act
    let body = get(&rig.app(), "/playlist/videos?playlist=2&page=1&perPage=1").await.json();
    // Assert
    assert_eq!(fetched_lists(&rig), ["PLccc"]);
    assert_eq!(
        body,
        json!({ "videos": [{ "index": 0, "id": "v1", "title": "Title v1", "duration": 100.0 }], "total": 2, "page": 1, "perPage": 1, "currentIndex": null, "playing": false })
    );
}

#[tokio::test]
async fn playlist_videos_marks_the_list_on_air_wherever_it_is_in_the_config() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({ "playing": { "index": 1, "playlistId": "PLbbb" } }))));
    rig.ytdlp.push(Ok(listing(&[("v1", 100.0), ("v2", 200.0)])));
    // Act
    let body = get(&rig.app(), "/playlist/videos?playlist=1&page=1&perPage=1").await.json();
    // Assert
    assert_eq!((body["currentIndex"].as_u64(), body["playing"].as_bool()), (Some(1), Some(true)));
}

#[tokio::test]
async fn playlist_videos_gives_the_first_list_no_current_video_while_another_is_on_air() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({ "playing": { "index": 1, "playlistId": "PLbbb" } }))));
    rig.ytdlp.push(Ok(listing(&[("v1", 100.0)])));
    // Act
    let body = get(&rig.app(), "/playlist/videos?playlist=0&page=1&perPage=1").await.json();
    // Assert
    assert!(body["currentIndex"].is_null());
    assert_eq!(body["playing"], false);
}

#[tokio::test]
async fn playlist_videos_rejects_a_playlist_that_is_not_a_configured_number() {
    for playlist in ["3", "-1", "x", "1.5", "", "01x"] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = get(&rig.app(), &format!("/playlist/videos?playlist={playlist}")).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid playlist index".to_string()), "{playlist}");
        assert!(rig.ytdlp.calls().is_empty());
    }
}

#[tokio::test]
async fn playlist_videos_takes_the_first_list_as_on_air_when_the_supervisor_names_none() {
    // Arrange
    let rig = Rig::new();
    rig.sup.set_status(status_of(&playing(json!({ "playing": { "index": 1 } }))));
    rig.ytdlp.push(Ok(listing(&[("v1", 100.0), ("v2", 200.0)])));
    // Act
    let body = get(&rig.app(), "/playlist/videos?page=1&perPage=1").await.json();
    // Assert
    assert_eq!(fetched_lists(&rig), ["PLaaa"]);
    assert_eq!((body["total"].as_u64(), body["currentIndex"].as_u64(), body["playing"].as_bool()), (Some(2), Some(1), Some(true)));
}

#[tokio::test]
async fn playlist_videos_pages_with_the_dashboards_defaults_and_bounds() {
    // Arrange
    let rig = Rig::new();
    let videos: Vec<(String, f64)> = (0..30).map(|i| (format!("v{i}"), 10.0)).collect();
    let refs: Vec<(&str, f64)> = videos.iter().map(|(id, d)| (id.as_str(), *d)).collect();
    rig.ytdlp.push(Ok(listing(&refs)));
    let app = rig.app();
    // Act
    let defaults = get(&app, "/playlist/videos").await.json();
    let second = get(&app, "/playlist/videos?page=2&perPage=25").await.json();
    let bounded = get(&app, "/playlist/videos?page=0&perPage=500").await.json();
    // Assert
    assert_eq!((defaults["page"].as_u64(), defaults["perPage"].as_u64(), defaults["videos"].as_array().unwrap().len()), (Some(1), Some(25), 25));
    assert_eq!(second["videos"].as_array().unwrap().len(), 5);
    assert_eq!(second["videos"][0]["id"], "v25");
    assert_eq!((bounded["page"].as_u64(), bounded["perPage"].as_u64()), (Some(1), Some(100)));
}

#[tokio::test]
async fn playlist_videos_answers_500_when_the_playlist_cannot_be_read() {
    // Arrange
    let rig = Rig::new();
    rig.ytdlp.push(Err(RunError { kind: RunErrorKind::Wait, message: "bot check".into() }));
    // Act
    let r = get(&rig.app(), "/playlist/videos").await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch playlist metadata".to_string()));
}

// Setup check.

#[tokio::test]
async fn setup_check_is_null_before_the_first_run() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = get(&rig.app(), "/setup-check").await;
    // Assert
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "null"));
}

#[tokio::test]
async fn setup_check_run_runs_the_check_on_the_install_root_then_get_returns_the_stored_report() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    // Act
    let report = post(&app, "/setup-check/run", json!({})).await.json();
    write(&rig.layout.setup_report, report.to_string().as_bytes());
    let stored = get(&app, "/setup-check").await.json();
    // Assert
    assert_eq!(*rig.setup_runs.lock().unwrap(), [(rig.layout.root.clone(), vec![])]);
    assert_eq!(report["encoder"]["chosen"], "h264_nvenc");
    assert_eq!(stored, report);
}

#[tokio::test]
async fn setup_check_run_sizes_the_cache_from_the_playing_playlists_durations() {
    // Arrange
    let rig = Rig::new();
    rig.ytdlp.push(Ok(listing(&[("a", 100.0), ("b", 30_474.0), ("c", 26_953.0)])));
    // Act
    post(&rig.app(), "/setup-check/run", json!({})).await;
    // Assert
    assert_eq!(fetched_lists(&rig), ["PLaaa"]);
    assert_eq!(rig.setup_runs.lock().unwrap()[0].1, [100.0, 30_474.0, 26_953.0]);
}

#[tokio::test]
async fn setup_check_run_uses_the_wizards_playlist_before_it_is_saved() {
    // Arrange
    let rig = Rig::new();
    // Act
    post(&rig.app(), "/setup-check/run", json!({ "playlistId": "PLwizardPlaylist01" })).await;
    // Assert
    assert_eq!(fetched_lists(&rig), ["PLwizardPlaylist01"]);
}

#[tokio::test]
async fn setup_check_run_refuses_a_playlist_id_that_is_not_one() {
    for id in [json!("../x"), json!("x"), Value::Null, json!(7)] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), "/setup-check/run", json!({ "playlistId": id })).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "Invalid playlist id".to_string()), "{id}");
        assert!(rig.setup_runs.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn setup_check_run_still_runs_when_the_playlist_cannot_be_read() {
    // Arrange
    let rig = Rig::new();
    rig.ytdlp.push(Err(RunError { kind: RunErrorKind::Wait, message: "bot check".into() }));
    // Act
    let r = post(&rig.app(), "/setup-check/run", json!({})).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(*rig.setup_runs.lock().unwrap(), [(rig.layout.root.clone(), vec![])]);
}

#[tokio::test]
async fn setup_check_run_clicked_twice_while_it_runs_probes_once() {
    // Arrange
    let rig = Rig::new();
    let gate = Arc::new(tokio::sync::Notify::new());
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (g, n) = (gate.clone(), runs.clone());
    let app = rig.app_with(move |d| {
        d.setup_check = Arc::new(move |_layout, _durations| {
            n.fetch_add(1, Ordering::SeqCst);
            let g = g.clone();
            Box::pin(async move {
                g.notified().await;
                Ok(report())
            })
        })
    });
    // Act
    let first = tokio::spawn({
        let app = app.clone();
        async move { post(&app, "/setup-check/run", json!({})).await }
    });
    eventually("the first run", || runs.load(Ordering::SeqCst) == 1).await;
    let second = tokio::spawn({
        let app = app.clone();
        async move { post(&app, "/setup-check/run", json!({})).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    gate.notify_waiters();
    let (a, b) = (first.await.unwrap(), second.await.unwrap());
    // Assert
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!((a.status, b.status), (StatusCode::OK, StatusCode::OK));
    assert_eq!(a.json(), b.json());
}

#[tokio::test]
async fn setup_check_run_answers_500_when_the_check_fails() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app_with(|d| d.setup_check = Arc::new(|_layout, _durations| Box::pin(async { Err("no free-space answer".to_string()) })));
    // Act
    let r = post(&app, "/setup-check/run", json!({})).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::INTERNAL_SERVER_ERROR, "no free-space answer".to_string()));
}

#[tokio::test]
async fn the_default_setup_check_probes_ffmpeg_measures_free_space_and_saves_the_report() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    std::fs::create_dir_all(&layout.data).unwrap();
    let runner = Arc::new(FakeRunner::new());
    runner.push(Ok(RunOutput::ok("")));
    let run = setup_check_with(runner.clone());
    // Act
    let report = run(layout.clone(), vec![]).await.unwrap();
    // Assert
    assert_eq!(runner.calls()[0].program, layout.ffmpeg.to_string_lossy());
    assert_eq!(report.encoder.chosen.as_str(), "h264_nvenc");
    assert!(report.cache.free_mb > 0);
    assert!(layout.setup_report.exists());
}

#[test]
fn free_space_is_measured_in_whole_megabytes() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    // Act
    let mb = streamloop_control::api::free_mb(dir.path()).unwrap();
    // Assert
    assert!(mb > 0);
    assert!(streamloop_control::api::free_mb(&dir.path().join("missing")).is_err());
}

// The API token.

#[tokio::test]
async fn refuses_changing_posts_without_the_api_token() {
    for path in ["/stream/stop", "/stream/start", "/stream-key", "/playlist/switch", "/player/next", "/config", "/setup-check/run"] {
        // Arrange
        let rig = Rig::new();
        let before = rig.config_text();
        // Act
        let r = post_without_token(&rig.app(), path, json!({ "key": KEY, "playlistIndex": 1 })).await;
        // Assert
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!((rig.launcher.marks(), rig.launcher.launches()), (0, 0), "{path}");
        assert!(rig.protected.lock().unwrap().is_empty(), "{path}");
        assert_eq!(rig.config_text(), before, "{path}");
        assert!(rig.sup.calls().is_empty(), "{path}");
    }
}

// During an update.

fn running_state() -> Value {
    json!({
        "from": "3.0.5", "to": "3.0.6", "parts": [], "folders": [], "step": "check", "result": "running",
        "reason": null, "startedAt": "2026-11-08T10:00:00.000Z", "stepStartedAt": "2026-11-08T10:02:00.000Z", "baseline": null
    })
}

fn changing_routes() -> Vec<(&'static str, Value)> {
    vec![
        ("/stream/stop", json!({})),
        ("/stream/start", json!({})),
        ("/player/next", json!({})),
        ("/player/prev", json!({})),
        ("/player/restart", json!({})),
        ("/player/jump", json!({ "index": 1 })),
        ("/playlist/switch", json!({ "playlistIndex": 1 })),
        ("/config", json!({ "autoOpenAdmin": false })),
        ("/stream-key", json!({ "key": KEY })),
        ("/update/apply", json!({})),
        ("/yt-dlp/update", json!({})),
        ("/setup-check/run", json!({})),
    ]
}

async fn expect_refused(rig: &Rig) {
    let app = rig.app();
    let before = rig.config_text();
    for (path, body) in changing_routes() {
        let r = post(&app, path, body).await;
        assert_eq!((path, r.status, r.error()), (path, StatusCode::CONFLICT, "An update is running. Try again when it is done.".to_string()));
    }
    assert_eq!((rig.launcher.marks(), rig.launcher.launches()), (0, 0));
    assert!(controls(rig).is_empty());
    assert!(reloads(rig).is_empty());
    assert_eq!(shutdowns(rig), 0);
    assert_eq!(rig.config_text(), before);
    assert!(rig.protected.lock().unwrap().is_empty());
    assert_eq!(rig.updates.lands(), 0);
    assert!(rig.setup_runs.lock().unwrap().is_empty());
    assert!(rig.runner.calls().is_empty());
}

#[tokio::test]
async fn refuses_stop_start_controls_and_saves_while_an_update_runs() {
    // Arrange: the new version is being confirmed; this process lands nothing.
    let rig = Rig::new();
    write(&rig.layout.update_state, running_state().to_string().as_bytes());
    rig.sup.set_status(status_of(&playing(json!({}))));
    // Act + Assert
    expect_refused(&rig).await;
}

#[tokio::test]
async fn takes_stop_while_a_landed_update_is_confirmed_and_refuses_the_other_changing_routes() {
    // Arrange: this process confirms the version it runs, with the progress still at the check
    let rig = Rig::new();
    write(&rig.layout.update_state, running_state().to_string().as_bytes());
    rig.updates.set_confirming(true);
    rig.sup.set_status(status_of(&playing(json!({}))));
    let app = rig.app();
    // Act
    let stop = post(&app, "/stream/stop", json!({})).await;
    let mut others = Vec::new();
    for (path, body) in changing_routes().into_iter().filter(|(p, _)| *p != "/stream/stop") {
        others.push((path, post(&app, path, body).await.status));
    }
    // Assert
    assert_eq!(stop.status, StatusCode::OK);
    assert_eq!((rig.launcher.marks(), shutdowns(&rig)), (1, 1));
    assert_eq!(others.len(), changing_routes().len() - 1);
    assert!(others.iter().all(|(_, s)| *s == StatusCode::CONFLICT), "{others:?}");
    assert_eq!(rig.launcher.launches(), 0);
}

#[tokio::test]
async fn refuses_stop_while_the_landing_itself_runs() {
    // Arrange: Update Now downloads the exe; the progress says running, and no confirmation runs
    let rig = Rig::new();
    let mut downloading = running_state();
    downloading["step"] = json!("download");
    write(&rig.layout.update_state, downloading.to_string().as_bytes());
    // Act
    let stop = post(&rig.app(), "/stream/stop", json!({})).await;
    // Assert
    assert_eq!((stop.status, stop.error()), (StatusCode::CONFLICT, "An update is running. Try again when it is done.".to_string()));
    assert_eq!(rig.launcher.marks(), 0);
}

#[tokio::test]
async fn refuses_them_while_a_landing_runs() {
    // Arrange
    let rig = Rig::new();
    rig.updates.set_landing(true);
    rig.updates.set_status(streamloop_control::updater::UpdateInfo { status: UpdateStatus::Ready, ..idle_update() });
    // Act + Assert
    expect_refused(&rig).await;
}

#[tokio::test]
async fn takes_commands_while_the_exe_stages_in_the_background() {
    // Arrange
    let rig = Rig::new();
    rig.updates.set_status(streamloop_control::updater::UpdateInfo { status: UpdateStatus::Downloading, ..idle_update() });
    let app = rig.app();
    // Act
    let next = post(&app, "/player/next", json!({})).await;
    let stop = post(&app, "/stream/stop", json!({})).await;
    // Assert
    assert_eq!((next.status, stop.status), (StatusCode::OK, StatusCode::OK));
    assert_eq!(controls(&rig).len(), 1);
    assert_eq!(rig.launcher.marks(), 1);
}

#[tokio::test]
async fn refuses_to_apply_while_an_update_check_runs_and_still_takes_stop() {
    // Arrange
    let rig = Rig::new();
    rig.updates.set_status(streamloop_control::updater::UpdateInfo { status: UpdateStatus::Checking, ..idle_update() });
    let app = rig.app();
    // Act
    let apply = post(&app, "/update/apply", json!({})).await;
    let stop = post(&app, "/stream/stop", json!({})).await;
    // Assert
    assert_eq!((apply.status, apply.error()), (StatusCode::CONFLICT, "An update is running. Try again when it is done.".to_string()));
    assert_eq!(rig.updates.lands(), 0);
    assert_eq!(stop.status, StatusCode::OK);
}

#[tokio::test]
async fn takes_commands_again_once_the_update_has_a_result() {
    // Arrange
    let rig = Rig::new();
    let mut done = running_state();
    done["step"] = json!("done");
    done["result"] = json!("ok");
    write(&rig.layout.update_state, done.to_string().as_bytes());
    // Act
    let r = post(&rig.app(), "/stream/start", json!({})).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(rig.launcher.launches(), 1);
}

#[tokio::test]
async fn update_apply_refuses_in_dev_mode_or_without_an_update_and_restarts_nothing() {
    let rows = [
        (streamloop_control::updater::UpdateInfo { is_dev_mode: true, ..idle_update() }, "Cannot apply updates in dev mode"),
        (streamloop_control::updater::UpdateInfo { update_available: false, latest_version: Some("3.0.5".into()), ..idle_update() }, "No update available to apply"),
    ];
    for (status, error) in rows {
        // Arrange
        let rig = Rig::new();
        rig.updates.set_status(status);
        // Act
        let r = post(&rig.app(), "/update/apply", json!({})).await;
        // Assert
        assert_eq!((r.status, r.error()), (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()));
        assert_eq!(rig.updates.lands(), 0);
        assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn update_apply_answers_at_once() {
    // Arrange
    let rig = Rig::new();
    rig.updates.hold();
    // Act
    let r = post(&rig.app(), "/update/apply", json!({})).await;
    // Assert
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, r#"{"ok":true,"message":"Update started"}"#));
    eventually("the landing to start", || rig.updates.lands() == 1).await;
    assert_eq!(rig.updates.landed(), 0);
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
    rig.updates.release();
    eventually("the restart", || rig.restarts.load(Ordering::SeqCst) == 1).await;
}

#[tokio::test]
async fn update_apply_restarts_once_the_landing_went_through() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/update/apply", json!({})).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    eventually("the restart", || rig.restarts.load(Ordering::SeqCst) == 1).await;
    assert_eq!(rig.updates.lands(), 1);
}

#[tokio::test]
async fn a_landing_that_fails_restarts_nothing() {
    // Arrange
    let rig = Rig::new();
    rig.updates.set_land_result(Err("The new streamloop.exe could not be moved into place.".into()));
    // Act
    let r = post(&rig.app(), "/update/apply", json!({})).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    eventually("the landing to end", || rig.updates.landed() == 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn update_status_adds_the_progress_and_an_empty_swap_log() {
    // Arrange
    let rig = Rig::new();
    let mut running = running_state();
    running["parts"] = json!(["supervisor"]);
    running["folders"] = json!(["supervisor", "yt-dlp"]);
    write(&rig.layout.update_state, running.to_string().as_bytes());
    // Act
    let r = get(&rig.app(), "/update/status").await;
    // Assert
    let mut want = serde_json::to_value(idle_update()).unwrap();
    want["progress"] = running;
    want["swapLog"] = json!([]);
    assert_eq!(r.json(), want);
    assert!(r.body.starts_with("{\"currentVersion\":"), "{}", r.body);
}

#[tokio::test]
async fn update_status_has_no_progress_before_any_update() {
    // Arrange
    let rig = Rig::new();
    // Act
    let body = get(&rig.app(), "/update/status").await.json();
    // Assert
    assert!(body["progress"].is_null());
    assert_eq!(body["swapLog"], json!([]));
    assert_eq!(body["kind"], "app");
}

#[tokio::test]
async fn update_check_answers_the_updaters_status() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/update/check", json!({})).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), serde_json::to_value(idle_update()).unwrap());
    assert_eq!(rig.updates.checks(), 1);
}

// Discord.

#[tokio::test]
async fn discord_test_sends_the_test_message() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/discord/test", json!({})).await;
    // Assert
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true })));
    let sent = rig.notifier.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "Test notification from StreamLoop. If you see this, your webhook is working!");
}

#[tokio::test]
async fn discord_test_needs_a_webhook_url() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app();
    post(&app, "/config", json!({ "discord": { "webhookUrl": "" } })).await;
    // Act
    let r = post(&app, "/discord/test", json!({})).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::BAD_REQUEST, "No Discord webhook URL configured".to_string()));
    assert!(rig.notifier.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discord_defaults_lists_the_templates_and_variables() {
    // Arrange
    let rig = Rig::new();
    // Act
    let body = get(&rig.app(), "/discord/defaults").await.json();
    // Assert
    assert_eq!(body["templates"]["recovery"], "Recovery action: **{step}**");
    assert_eq!(body["variables"]["error"], json!(["videoIndex", "videoId", "errorCode", "attempt"]));
    assert_eq!(body["templates"].as_object().unwrap().len(), 10);
    assert_eq!(body["variables"].as_object().unwrap().len(), 10);
}

// yt-dlp.

#[tokio::test]
async fn yt_dlp_update_reports_the_version_and_refetches_playlists() {
    // Arrange
    let rig = Rig::new();
    write(&rig.layout.ytdlp, b"");
    rig.ytdlp.push(Ok(listing(&[("v1", 1.0)])));
    rig.ytdlp.push(Ok(listing(&[("v1", 1.0), ("v2", 2.0)])));
    rig.runner.push(Ok(RunOutput::ok("Updated")));
    rig.runner.push(Ok(RunOutput::ok("2026.11.01\n")));
    let app = rig.app();
    get(&app, "/playlist/videos").await;
    // Act
    let r = post(&app, "/yt-dlp/update", json!({})).await;
    let after = get(&app, "/playlist/videos").await.json();
    // Assert
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true, "version": "2026.11.01" })));
    assert_eq!(rig.runner.calls()[0].program, rig.layout.ytdlp.with_file_name("yt-dlp-update.exe").to_string_lossy());
    assert!(rig.layout.ytdlp.with_file_name("yt-dlp-2026.11.01.exe").exists());
    assert_eq!(after["total"], 2);
}

#[tokio::test]
async fn yt_dlp_update_answers_500_with_the_reason() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/yt-dlp/update", json!({})).await;
    // Assert
    assert_eq!((r.status, r.error()), (StatusCode::INTERNAL_SERVER_ERROR, "yt-dlp binary not found (dev mode or missing install)".to_string()));
}

// Test hook, shutdown, removed routes.

#[tokio::test]
async fn has_no_exit_75_test_hook_unless_test_hooks_are_on() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/test/exit-75", json!({})).await;
    // Assert
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runs_the_update_restart_from_the_exit_75_test_hook_when_test_hooks_are_on() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app_with(|d| d.test_hooks = true);
    // Act
    let r = post(&app, "/test/exit-75", json!({})).await;
    // Assert
    assert_eq!((r.status, r.json()), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refuses_the_exit_75_test_hook_without_the_api_token() {
    // Arrange
    let rig = Rig::new();
    let app = rig.app_with(|d| d.test_hooks = true);
    // Act
    let r = call(&app, Request::builder().method(Method::POST).uri("/api/test/exit-75").body(Body::empty()).unwrap()).await;
    // Assert
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn shutdown_answers_then_stops_after_500_ms() {
    // Arrange
    let rig = Rig::new();
    // Act
    let r = post(&rig.app(), "/shutdown", json!({})).await;
    // Assert
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, r#"{"ok":true,"message":"Shutting down..."}"#));
    // Lets the spawned stop start its timer at the moment of the reply.
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(499)).await;
    tokio::task::yield_now().await;
    assert_eq!(rig.shutdowns.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(rig.shutdowns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn no_longer_serves_the_2x_routes() {
    for path in ["/player/seek", "/player/pause", "/player/stop", "/player/resume", "/player/restart-mpv", "/obs-path/detect", "/obs-path/validate"] {
        // Arrange
        let rig = Rig::new();
        // Act
        let r = post(&rig.app(), path, json!({})).await;
        // Assert
        assert_eq!((r.status, r.content_type.as_str()), (StatusCode::NOT_FOUND, "text/html; charset=utf-8"), "{path}");
        assert!(r.body.contains(&format!("<pre>Cannot POST /api{path}</pre>")), "{path}: {}", r.body);
    }
}

