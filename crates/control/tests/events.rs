use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use streamloop_common::config::Playlist;
use streamloop_control::clock::TestClock;
use streamloop_control::events::{
    describe, launcher_event, with_playlist_names, Alert, AlertGate, Described, DiscordKind, SupervisorEvent, SupervisorEvents, FFMPEG_MISSING, NO_ENCODER,
};

/// 2026-10-25 12:00 UTC: the same calendar day on every time zone from -11 to +11.
const NOON: u64 = 1_792_929_600;
const DAY: Duration = Duration::from_secs(86_400);

fn ev(kind: &str, extra: Value, level: &str) -> SupervisorEvent {
    let mut m = Map::new();
    m.insert("ts".into(), "2026-10-25T10:00:00Z".into());
    m.insert("level".into(), level.into());
    m.insert("kind".into(), kind.into());
    m.extend(extra.as_object().cloned().unwrap_or_default());
    SupervisorEvent::from_map(m).unwrap()
}

fn info(kind: &str, extra: Value) -> SupervisorEvent {
    ev(kind, extra, "info")
}

fn dsc(e: &SupervisorEvent) -> Described {
    describe(e)
}

fn alert(d: &Described) -> &Alert {
    d.discord.as_ref().expect("an alert")
}

fn var<'a>(a: &'a Alert, name: &str) -> &'a Value {
    &a.vars.iter().find(|(k, _)| k == name).unwrap().1
}

fn kind_of(d: &Described) -> Option<&'static str> {
    d.discord.as_ref().map(|a| a.kind.as_str())
}

fn described_value(d: &Described) -> Value {
    let mut m = Map::new();
    m.insert("message".into(), d.message.clone().into());
    if let Some(a) = &d.discord {
        let vars: Map<String, Value> = a.vars.iter().cloned().collect();
        m.insert("discord".into(), json!({ "kind": a.kind.as_str(), "vars": vars, "level": a.level.as_str() }));
    }
    Value::Object(m)
}

// ---------------------------------------------------------------- the tail

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn tail(dir: &Path) -> (SupervisorEvents, Arc<TestClock>) {
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(NOON)));
    (SupervisorEvents::new(dir, clock.clone()), clock)
}

fn line(kind: &str) -> String {
    format!("{}\n", serde_json::to_string(&ev(kind, json!({}), "info")).unwrap())
}

fn append(path: &Path, text: &str) {
    OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
}

fn kinds(events: Vec<SupervisorEvent>) -> Vec<String> {
    events.into_iter().map(|e| e.kind).collect()
}

fn legacy(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("events-{date}.jsonl"))
}

#[test]
fn reads_appended_lines_once_and_skips_history_on_first_start() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, line("old")).unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    // Act
    append(&f, &(line("a") + &line("b")));
    let first = t.poll();
    let second = t.poll();
    // Assert
    assert_eq!(kinds(first), ["a", "b"]);
    assert!(second.is_empty());
}

#[test]
fn follows_a_supervisor_that_switches_to_supervisor_events_mid_day_and_reads_the_new_file_from_its_start() {
    // Arrange
    let d = dir();
    let old = legacy(d.path(), "2026-10-25");
    fs::write(&old, line("old-history") + &line("old-history")).unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    append(&old, &line("old-last"));
    let mut seen = kinds(t.poll());
    // Act
    fs::write(d.path().join("supervisor-events-2026-10-25.jsonl"), line("new-first")).unwrap();
    seen.extend(kinds(t.poll()));
    // Assert
    assert_eq!(seen, ["old-last", "new-first"]);
}

#[test]
fn holds_a_half_written_line_until_it_is_finished() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, "").unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    let full = line("late");
    // Act
    append(&f, &full[..10]);
    let held = t.poll();
    append(&f, &full[10..]);
    let done = t.poll();
    // Assert
    assert!(held.is_empty());
    assert_eq!(kinds(done), ["late"]);
}

#[test]
fn finishes_yesterday_then_reads_today_from_the_start_at_midnight() {
    // Arrange
    let d = dir();
    let y = legacy(d.path(), "2026-10-25");
    fs::write(&y, "").unwrap();
    let (mut t, clock) = tail(d.path());
    t.start();
    append(&y, &line("last-of-day"));
    fs::write(legacy(d.path(), "2026-10-26"), line("first-of-day")).unwrap();
    clock.advance(DAY);
    // Act
    let seen = kinds(t.poll());
    // Assert
    assert_eq!(seen, ["last-of-day", "first-of-day"]);
}

#[test]
fn reads_a_file_that_does_not_exist_yet_from_its_start_once_it_appears() {
    // Arrange
    let d = dir();
    let (mut t, _) = tail(d.path());
    t.start();
    let before = t.poll();
    // Act
    fs::write(legacy(d.path(), "2026-10-25"), line("born")).unwrap();
    let after = kinds(t.poll());
    // Assert
    assert!(before.is_empty());
    assert_eq!(after, ["born"]);
}

#[test]
fn skips_an_unparseable_line_in_the_middle_of_a_batch() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, "").unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    // Act
    append(&f, &(line("a") + "not json\n" + &line("b")));
    // Assert
    assert_eq!(kinds(t.poll()), ["a", "b"]);
}

#[test]
fn skips_lines_that_are_not_objects_with_a_string_kind() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, "").unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    // Act
    append(&f, &(line("a") + "5\nnull\n[]\n{\"kind\":7}\n{\"ts\":\"x\"}\n\n   \n" + &line("b")));
    // Assert
    assert_eq!(kinds(t.poll()), ["a", "b"]);
}

#[test]
fn does_not_replay_yesterday_or_today_when_started_just_after_midnight() {
    // Arrange
    let d = dir();
    fs::write(legacy(d.path(), "2026-10-25"), line("yesterday")).unwrap();
    let today = legacy(d.path(), "2026-10-26");
    fs::write(&today, line("earlier-today")).unwrap();
    let (mut t, clock) = tail(d.path());
    clock.advance(DAY);
    t.start();
    // Act
    let idle = t.poll();
    append(&today, &line("new"));
    let seen = kinds(t.poll());
    // Assert
    assert!(idle.is_empty());
    assert_eq!(seen, ["new"]);
}

#[test]
fn rereads_from_the_start_when_the_file_shrank() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, line("before") + &line("before2")).unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    // Act
    fs::write(&f, line("new")).unwrap();
    // Assert
    assert_eq!(kinds(t.poll()), ["new"]);
}

#[test]
fn rereads_a_recreated_file_that_is_shorter_than_the_old_offset() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, line("old") + &line("old")).unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    // Act
    fs::remove_file(&f).unwrap();
    let gone = t.poll();
    fs::write(&f, line("fresh")).unwrap();
    let back = kinds(t.poll());
    // Assert
    assert!(gone.is_empty());
    assert_eq!(back, ["fresh"]);
}

#[test]
fn polls_quietly_when_the_logs_folder_does_not_exist() {
    // Arrange
    let d = dir();
    let missing = d.path().join("no-such-logs");
    let (mut t, clock) = tail(&missing);
    // Act
    t.start();
    let first = t.poll();
    clock.advance(DAY);
    let second = t.poll();
    // Assert
    assert!(first.is_empty());
    assert!(second.is_empty());
}

#[test]
fn prefers_the_supervisor_events_name_when_both_exist_after_the_day_rolls_over() {
    // Arrange
    let d = dir();
    fs::write(legacy(d.path(), "2026-10-25"), "").unwrap();
    let (mut t, clock) = tail(d.path());
    t.start();
    fs::write(legacy(d.path(), "2026-10-26"), line("legacy")).unwrap();
    fs::write(d.path().join("supervisor-events-2026-10-26.jsonl"), line("current")).unwrap();
    clock.advance(DAY);
    // Act
    let seen = kinds(t.poll());
    // Assert
    assert_eq!(seen, ["current"]);
}

#[test]
fn start_twice_keeps_the_first_offset() {
    // Arrange
    let d = dir();
    let f = legacy(d.path(), "2026-10-25");
    fs::write(&f, "").unwrap();
    let (mut t, _) = tail(d.path());
    t.start();
    append(&f, &line("a"));
    // Act
    t.start();
    // Assert
    assert_eq!(kinds(t.poll()), ["a"]);
}

// ---------------------------------------------------------------- describe

#[test]
fn maps_encoder_exit_to_stream_drop_and_a_third_failure_to_critical() {
    // Arrange
    let warn = ev("encoder-exit", json!({"why": "crash", "failures": 1, "restartInS": 2}), "warn");
    let crit = ev("encoder-exit", json!({"why": "crash", "failures": 3, "restartInS": 5}), "critical");
    // Act
    let (w, c) = (dsc(&warn), dsc(&crit));
    // Assert
    assert_eq!((alert(&w).kind, alert(&w).level.as_str()), (DiscordKind::StreamDrop, "warn"));
    assert_eq!(var(alert(&w), "attempt"), &json!(1));
    assert_eq!((alert(&c).kind, alert(&c).level.as_str()), (DiscordKind::Critical, "error"));
}

#[test]
fn notes_a_token_cache_clear_on_the_timeline_only() {
    // Arrange
    let cleared = info("token-caches-cleared", json!({"refusals": 2, "ok": true}));
    let failed = ev("token-caches-cleared", json!({"refusals": 2, "ok": false}), "warn");
    // Act
    let (c, f) = (dsc(&cleared), dsc(&failed));
    // Assert
    assert_eq!(c, Described { message: "YouTube refused 2 downloads with a token, so the token server starts over with new tokens".into(), discord: None });
    assert_eq!(f.message, "YouTube refused 2 downloads with a token, and the token server's cache could not be cleared");
    assert!(f.discord.is_none());
}

#[test]
fn says_when_a_failed_download_is_tried_again() {
    // Arrange
    let reason = "ERROR: unable to download video data: HTTP Error 403: Forbidden";
    // Act
    let scheduled = dsc(&ev("download-failed", json!({"index": 165, "reason": reason, "retryInS": 3600}), "warn"));
    let old = dsc(&ev("download-failed", json!({"index": 165, "reason": reason}), "warn"));
    // Assert
    assert_eq!(scheduled.message, format!("Download of video 166 failed: {reason}; trying again in 60 min"));
    assert_eq!(old.message, format!("Download of video 166 failed: {reason}"));
}

#[test]
fn maps_encoder_start_to_stream_restart_only_after_an_exit() {
    // Arrange / Act
    let first = dsc(&info("encoder-start", json!({"generation": 1, "restarts": 0})));
    let again = dsc(&info("encoder-start", json!({"generation": 2, "restarts": 1})));
    // Assert
    assert!(first.discord.is_none());
    assert_eq!(alert(&again).kind, DiscordKind::StreamRestart);
    assert_eq!(var(alert(&again), "attempts"), &json!(1));
}

#[test]
fn maps_skip_retry_and_resume_with_1_based_video_numbers() {
    // Arrange / Act
    let skip = dsc(&ev("skip", json!({"index": 3, "id": "abc", "why": "unavailable", "consecutive": 1}), "warn"));
    let retry = dsc(&ev("retry", json!({"index": 0, "id": "x", "why": "stall", "retry": 2, "pos": 12.5}), "warn"));
    let resume = dsc(&info("resume", json!({"index": 1, "id": "r", "pos": 30})));
    // Assert
    assert_eq!(alert(&skip).kind, DiscordKind::Skip);
    assert_eq!(described_value(&skip)["discord"]["vars"], json!({"videoIndex": 4, "videoId": "abc", "reason": "unavailable"}));
    assert_eq!(alert(&retry).kind, DiscordKind::Error);
    assert_eq!(described_value(&retry)["discord"]["vars"], json!({"videoIndex": 1, "videoId": "x", "errorCode": "stall", "attempt": 2}));
    assert_eq!(retry.message, "Retrying video 1 at 12s (stall, attempt 2)");
    assert_eq!(alert(&resume).kind, DiscordKind::Resume);
    assert_eq!(described_value(&resume)["discord"]["vars"], json!({"videoIndex": 2, "videoId": "r"}));
}

#[test]
fn sends_a_third_skip_in_a_row_as_critical() {
    // Arrange / Act
    let d = dsc(&ev("skip", json!({"index": 2, "id": "z", "why": "x", "consecutive": 3}), "critical"));
    // Assert
    assert_eq!(kind_of(&d), Some("critical"));
}

#[test]
fn sends_watchdog_retries_and_skips_to_output_frozen_content_and_ignored_to_the_timeline_only() {
    // Arrange
    let w = |kind: &str, outcome: &str| dsc(&ev(kind, json!({"index": 1, "outcome": outcome}), "warn"));
    // Act
    let retry = w("watchdog-freeze", "retry");
    let skip = w("watchdog-silence", "skip");
    let content = w("watchdog-silence", "content");
    let ignored = w("watchdog-freeze", "ignored");
    // Assert
    assert_eq!(alert(&retry).kind, DiscordKind::OutputFrozen);
    assert_eq!(described_value(&retry)["discord"]["vars"], json!({"kind": "freeze", "videoIndex": 2}));
    assert_eq!(var(alert(&skip), "kind"), &json!("silence"));
    assert!(content.discord.is_none());
    assert!(ignored.discord.is_none());
    assert!(!content.message.is_empty());
}

#[test]
fn describes_a_silence_as_the_video_playing_on_never_as_a_fault() {
    // Arrange / Act
    let d = dsc(&info("watchdog-silence", json!({"index": 1, "outcome": "content"})));
    // Assert
    assert_eq!(d, Described { message: "Silence on video 2. Silence alone never restarts a video.".into(), discord: None });
}

#[test]
fn sends_each_listed_kind_as_critical() {
    // Arrange
    let rows = [
        ("all-skipped", json!({})),
        ("stream-key-unreadable", json!({"reason": "r"})),
        ("stream-key-missing", json!({})),
        ("youtube-breaker-open", json!({"reason": "bot"})),
        ("downloads-failing", json!({"inARow": 3})),
        ("feeder-spawn-failed", json!({"index": 0, "error": "e"})),
        ("encoder-spawn-failed", json!({"error": "e"})),
        ("supervisor-down", json!({})),
        ("supervisor-missing", json!({})),
    ];
    for (kind, extra) in rows {
        // Act
        let d = dsc(&ev(kind, extra, "critical"));
        // Assert
        assert_eq!((kind_of(&d), alert(&d).level.as_str()), (Some("critical"), "error"), "{kind}");
    }
}

#[test]
fn names_the_browser_and_the_reason_when_yt_dlp_cannot_read_the_cookies() {
    // Arrange / Act
    let d = dsc(&ev("cookies-unreadable", json!({"browser": "firefox", "reason": "could not find firefox cookies database"}), "critical"));
    // Assert
    assert_eq!(d.message, "yt-dlp could not read the firefox cookies: could not find firefox cookies database");
    assert_eq!((alert(&d).kind, alert(&d).level.as_str()), (DiscordKind::Critical, "error"));
}

#[test]
fn says_the_saved_playlist_is_used_while_youtube_does_not_answer() {
    // Arrange / Act
    let d = dsc(&ev("playlist-from-saved", json!({"playlistId": "PLx", "count": 12}), "warn"));
    // Assert
    assert_eq!(d, Described { message: "YouTube is not answering, so the saved playlist (12 videos) is used.".into(), discord: None });
}

#[test]
fn says_a_finished_list_moved_on_to_the_next_one_by_name() {
    // Arrange / Act
    let d = dsc(&info("playlist-advance", json!({"from": "PLa", "to": "PLb", "name": "2021/2022", "count": 5, "finished": true})));
    // Assert
    assert_eq!(d, Described { message: "Playlist finished. Now playing 2021/2022".into(), discord: None });
}

#[test]
fn names_a_list_by_its_id_when_it_has_no_name() {
    // Arrange / Act
    let d = dsc(&info("playlist-advance", json!({"from": "PLa", "to": "PLb", "count": 5, "finished": true})));
    // Assert
    assert_eq!(d.message, "Playlist finished. Now playing PLb");
}

#[test]
fn says_another_list_is_on_air_after_a_jump_without_calling_the_first_finished() {
    // Arrange / Act
    let d = dsc(&info("playlist-advance", json!({"from": "PLa", "to": "PLb", "name": "2021/2022", "count": 5, "finished": false})));
    // Assert
    assert_eq!(d.message, "Now playing 2021/2022");
}

#[test]
fn says_a_list_that_went_round_starts_over() {
    // Arrange / Act
    let d = dsc(&info("playlist-restart", json!({"playlistId": "PLa", "count": 192})));
    // Assert
    assert_eq!(d, Described { message: "Playlist finished. Starting it again (192 videos)".into(), discord: None });
}

#[test]
fn alerts_when_a_list_starts_over_because_the_next_one_could_not_load() {
    // Arrange / Act
    let d = dsc(&ev("playlist-restart", json!({"playlistId": "PLa", "count": 192, "missing": ["2021/2022"]}), "critical"));
    // Assert
    assert_eq!(d.message, "Playlist finished. Starting it again, because 2021/2022 could not be loaded");
    assert_eq!(kind_of(&d), Some("critical"));
}

#[test]
fn alerts_when_a_list_moves_on_past_one_that_could_not_load() {
    // Arrange / Act
    let d = dsc(&ev("playlist-advance", json!({"from": "PLa", "to": "PLb", "name": "B", "count": 5, "finished": true, "missing": ["C", "D"]}), "critical"));
    // Assert
    assert_eq!(d.message, "Playlist finished. Now playing B. C, D could not be loaded");
    assert_eq!(kind_of(&d), Some("critical"));
}

#[test]
fn names_the_list_a_failed_load_was_for() {
    // Arrange / Act
    let d = dsc(&ev("playlist-failed", json!({"reason": "r", "playlistId": "PLb", "name": "2021/2022", "refetch": true}), "warn"));
    // Assert
    assert_eq!(d.message, "Playlist 2021/2022 could not be loaded: r");
}

#[test]
fn sends_playlist_failed_as_critical_only_for_the_first_load() {
    // Arrange / Act
    let first = dsc(&ev("playlist-failed", json!({"reason": "r"}), "critical"));
    let later = dsc(&ev("playlist-failed", json!({"reason": "r"}), "warn"));
    // Assert
    assert_eq!(kind_of(&first), Some("critical"));
    assert!(later.discord.is_none());
}

#[test]
fn maps_supervisor_back_to_an_info_alert_and_supervisor_launched_to_the_timeline() {
    // Arrange / Act
    let back = dsc(&info("supervisor-back", json!({})));
    let launched = dsc(&info("supervisor-launched", json!({})));
    // Assert
    assert_eq!((alert(&back).kind, alert(&back).level.as_str()), (DiscordKind::Recovery, "info"));
    assert_eq!(var(alert(&back), "step"), &json!("Supervisor answering again"));
    assert!(launched.discord.is_none());
}

#[test]
fn keeps_a_launch_that_broke_away_from_the_callers_job_off_the_timeline() {
    // Arrange / Act
    let d = dsc(&info("launched", json!({"breakaway": true, "pid": 1})));
    // Assert
    assert_eq!(d, Described { message: String::new(), discord: None });
}

#[test]
fn notes_a_launch_that_stayed_inside_the_callers_job() {
    // Arrange / Act
    let d = dsc(&info("launched", json!({"breakaway": false, "pid": 1})));
    // Assert
    assert!(d.message.contains("inside the caller's job"));
    assert!(d.discord.is_none());
}

#[test]
fn keeps_each_listed_kind_off_discord() {
    // Arrange
    let kinds = ["token-server-spawn", "token-server-down", "token-server-restart", "token-server-adopt", "reload", "start", "control", "playlist", "youtube-probe-failed"];
    for kind in kinds {
        // Act
        let d = dsc(&info(kind, json!({"error": "e", "reason": "r"})));
        // Assert
        assert!(d.discord.is_none(), "{kind}");
    }
}

#[test]
fn keeps_each_quiet_kind_out_of_the_timeline_and_discord() {
    // Arrange
    let kinds = ["feeder-start", "handover", "spare-resolve", "spare-used", "youtube-probe", "download-cancel"];
    for kind in kinds {
        // Act
        let d = dsc(&info(kind, json!({})));
        // Assert
        assert_eq!(d, Described { message: String::new(), discord: None }, "{kind}");
    }
}

#[test]
fn puts_an_unknown_kind_on_the_timeline_by_name() {
    // Arrange / Act
    let d = dsc(&info("something-new", json!({})));
    // Assert
    assert_eq!(d, Described { message: "something-new".into(), discord: None });
}

#[test]
fn trims_detail_to_one_line_of_at_most_200_characters() {
    // Arrange
    let detail = format!("first\n{}", "y".repeat(500));
    let long = format!("{}\r\nsecond", "z".repeat(500));
    // Act
    let a = dsc(&ev("skip", json!({"index": 0, "id": "a", "why": "x", "detail": detail, "consecutive": 1}), "warn"));
    let b = dsc(&ev("skip", json!({"index": 0, "id": "a", "why": long, "consecutive": 1}), "warn"));
    // Assert
    let reason = var(alert(&a), "reason").as_str().unwrap();
    assert_eq!(reason, "x: first");
    let long_reason = var(alert(&b), "reason").as_str().unwrap();
    assert_eq!(long_reason.chars().count(), 200);
    assert!(long_reason.ends_with("..."));
}

#[test]
fn builds_launcher_events_with_the_right_level() {
    // Arrange
    let now = UNIX_EPOCH + Duration::from_secs(NOON);
    // Act
    let down = launcher_event("supervisor-down", Map::new(), now);
    let back = launcher_event("supervisor-back", Map::new(), now);
    // Assert
    assert_eq!(down.level, "critical");
    assert_eq!(back.level, "info");
    assert_eq!(down.ts, "2026-10-25T12:00:00.000Z");
}

// ---------------------------------------------------------------- withPlaylistNames

fn lists() -> Vec<Playlist> {
    vec![Playlist { id: "PLa".into(), name: Some("first".into()) }, Playlist { id: "PLb".into(), name: None }]
}

#[test]
fn names_the_list_a_list_moved_on_to() {
    // Arrange / Act
    let e = with_playlist_names(info("playlist-advance", json!({"to": "PLa"})), &lists());
    // Assert
    assert_eq!(e.rest["name"], json!("first"));
}

#[test]
fn leaves_a_list_with_no_name_to_be_shown_by_its_id() {
    // Arrange / Act
    let e = with_playlist_names(info("playlist-advance", json!({"to": "PLb"})), &lists());
    // Assert
    assert!(!e.rest.contains_key("name"));
}

#[test]
fn names_the_list_a_failed_load_was_for_in_the_config() {
    // Arrange / Act
    let e = with_playlist_names(info("playlist-failed", json!({"playlistId": "PLa"})), &lists());
    // Assert
    assert_eq!(e.rest["name"], json!("first"));
}

#[test]
fn names_the_lists_that_could_not_load_by_id_where_they_have_no_name() {
    // Arrange / Act
    let e = with_playlist_names(info("playlist-restart", json!({"missing": ["PLa", "PLb", "PLgone"]})), &lists());
    // Assert
    assert_eq!(e.rest["missing"], json!(["first", "PLb", "PLgone"]));
}

#[test]
fn leaves_every_other_event_as_it_is() {
    // Arrange
    let e = info("skip", json!({"to": "PLa", "playlistId": "PLa"}));
    // Act
    let out = with_playlist_names(e.clone(), &lists());
    // Assert
    assert_eq!(out, e);
}

// ---------------------------------------------------------------- the freeze pair and detail rules

#[test]
fn sends_one_alert_for_a_watchdog_freeze_and_keeps_the_ladder_retry_on_the_timeline_only() {
    // Arrange / Act
    let w = dsc(&ev("watchdog-freeze", json!({"index": 1, "outcome": "retry"}), "warn"));
    let r = dsc(&ev("retry", json!({"index": 1, "id": "x", "why": "freeze", "retry": 1, "pos": 3}), "warn"));
    // Assert
    assert_eq!(kind_of(&w), Some("outputFrozen"));
    assert!(r.discord.is_none());
    assert!(!r.message.is_empty());
}

#[test]
fn keeps_the_ladder_skip_after_a_freeze_on_the_timeline_only_but_a_critical_streak_still_alerts() {
    // Arrange / Act
    let s = dsc(&ev("skip", json!({"index": 1, "id": "x", "why": "freeze", "consecutive": 1}), "warn"));
    let c = dsc(&ev("skip", json!({"index": 1, "id": "x", "why": "freeze", "consecutive": 3}), "critical"));
    // Assert
    assert!(s.discord.is_none());
    assert_eq!(kind_of(&c), Some("critical"));
}

#[test]
fn sends_an_unknown_critical_kind_to_discord_as_critical() {
    // Arrange / Act
    let d = dsc(&ev("brand-new", json!({}), "critical"));
    // Assert
    assert_eq!((alert(&d).kind, alert(&d).level.as_str()), (DiscordKind::Critical, "error"));
}

#[test]
fn uses_only_a_string_detail_reason_never_a_stringified_object() {
    // Arrange / Act
    let obj = dsc(&ev("skip", json!({"index": 0, "id": "a", "why": "other", "detail": {"since": 12.3, "pos": 40}, "consecutive": 1}), "warn"));
    let text = dsc(&ev("skip", json!({"index": 0, "id": "a", "why": "unavailable", "detail": {"reason": "private video"}, "consecutive": 1}), "warn"));
    // Assert
    assert_eq!(var(alert(&obj), "reason"), &json!("other"));
    assert_eq!(var(alert(&text), "reason"), &json!("unavailable: private video"));
}

// ---------------------------------------------------------------- AlertGate

fn gate_kinds(g: &mut AlertGate, es: Vec<SupervisorEvent>) -> Vec<Option<&'static str>> {
    es.iter().map(|e| kind_of(&g.describe(e))).collect()
}

fn exit(failures: u32, level: &str) -> SupervisorEvent {
    ev("encoder-exit", json!({"why": "x", "failures": failures, "restartInS": 1}), level)
}

#[test]
fn alerts_a_refused_config_once_per_streak_again_after_a_supervisor_that_started() {
    // Arrange
    let mut gate = AlertGate::default();
    let refused = ev("config-refused", json!({"reason": "config.json: ytdlCookiesFromBrowser is longer than 200 characters"}), "critical");
    // Act
    let sent = gate_kinds(&mut gate, vec![refused.clone(), refused.clone(), refused.clone(), info("start", json!({})), refused.clone()]);
    // Assert
    assert_eq!(sent, [Some("critical"), None, None, None, Some("critical")]);
    assert_eq!(
        gate.describe(&refused).message,
        "The supervisor refused config.json: config.json: ytdlCookiesFromBrowser is longer than 200 characters"
    );
}

#[test]
fn notes_cookies_yt_dlp_went_on_without_as_a_warning_off_discord() {
    // Arrange / Act
    let d = dsc(&ev("cookies-unreadable", json!({"browser": "chrome", "reason": "Failed to decrypt with DPAPI"}), "warn"));
    // Assert
    assert_eq!(d, Described { message: "yt-dlp could not read the chrome cookies and went on without them: Failed to decrypt with DPAPI".into(), discord: None });
}

#[test]
fn alerts_once_per_outage_one_drop_one_critical_one_restart_every_cycle_on_the_timeline() {
    // Arrange
    let mut gate = AlertGate::default();
    let mut out = Vec::new();
    // Act
    for i in 1..=20 {
        out.push(gate.describe(&exit(i, if i >= 3 { "critical" } else { "warn" })));
    }
    out.push(gate.describe(&info("encoder-start", json!({"generation": 21, "restarts": 20}))));
    // Assert
    let sent: Vec<_> = out.iter().filter_map(kind_of).collect();
    assert_eq!(sent, ["streamDrop", "critical", "streamRestart"]);
    assert!(out.iter().all(|d| !d.message.is_empty()));
    assert_eq!(out.len(), 21);
}

#[test]
fn starts_a_new_streak_after_the_restart() {
    // Arrange
    let mut gate = AlertGate::default();
    // Act
    let got = gate_kinds(
        &mut gate,
        vec![exit(1, "warn"), exit(2, "warn"), exit(3, "critical"), exit(4, "critical"), info("encoder-start", json!({"restarts": 4})), exit(1, "warn")],
    );
    // Assert
    assert_eq!(got, [Some("streamDrop"), None, Some("critical"), None, Some("streamRestart"), Some("streamDrop")]);
}

#[test]
fn alerts_on_the_first_encoder_spawn_failed_since_the_last_start_only_then_one_restart() {
    // Arrange
    let mut gate = AlertGate::default();
    let fail = || ev("encoder-spawn-failed", json!({"error": "e"}), "critical");
    // Act
    let got = gate_kinds(&mut gate, vec![fail(), fail(), fail(), info("encoder-start", json!({"restarts": 0})), fail()]);
    // Assert
    assert_eq!(got, [Some("critical"), None, None, Some("streamRestart"), Some("critical")]);
}

#[test]
fn treats_a_drop_after_48_hours_of_encoding_as_twitch_ending_the_stream_not_an_outage() {
    // Arrange
    let cut = ev("encoder-exit", json!({"why": "exited (exit code: 0xffffd8bb)", "code": -10053, "failures": 1, "restartInS": 2, "uptimeS": 172_802}), "warn");
    let mut gate = AlertGate::default();
    // Act
    let got = gate_kinds(&mut gate, vec![cut.clone(), info("encoder-start", json!({"generation": 1, "restarts": 1}))]);
    // Assert
    assert_eq!(got, [None, None]);
    assert_eq!(dsc(&cut).message, "Twitch ended the stream at its 48 hour limit; restarting in 2s");
}

#[test]
fn still_alerts_when_the_restart_after_the_48_hour_cut_keeps_failing() {
    // Arrange
    let mut gate = AlertGate::default();
    let es = vec![
        ev("encoder-exit", json!({"why": "x", "failures": 1, "restartInS": 2, "uptimeS": 172_802}), "warn"),
        ev("encoder-exit", json!({"why": "x", "failures": 2, "restartInS": 5, "uptimeS": 1}), "warn"),
        ev("encoder-exit", json!({"why": "x", "failures": 3, "restartInS": 10, "uptimeS": 1}), "critical"),
        info("encoder-start", json!({"restarts": 4})),
    ];
    // Act
    let got = gate_kinds(&mut gate, es);
    // Assert
    assert_eq!(got, [None, None, Some("critical"), Some("streamRestart")]);
}

#[test]
fn sends_no_restart_for_a_start_that_follows_no_alert() {
    // Arrange / Act
    let d = AlertGate::default().describe(&info("encoder-start", json!({"restarts": 2})));
    // Assert
    assert!(d.discord.is_none());
}

#[test]
fn alerts_token_server_spawn_failed_once_until_the_next_token_server_spawn() {
    // Arrange
    let mut gate = AlertGate::default();
    let fail = || ev("token-server-spawn-failed", json!({"error": "e"}), "critical");
    // Act
    let got = gate_kinds(&mut gate, vec![fail(), fail(), info("token-server-spawn", json!({})), fail()]);
    // Assert
    assert_eq!(got, [Some("critical"), None, None, Some("critical")]);
}

#[test]
fn re_arms_the_token_alert_when_the_supervisor_adopts_a_running_token_server() {
    // Arrange
    let mut gate = AlertGate::default();
    let fail = || ev("token-server-spawn-failed", json!({"error": "e"}), "critical");
    // Act
    let got = gate_kinds(&mut gate, vec![fail(), info("token-server-adopt", json!({})), fail()]);
    // Assert
    assert_eq!(got, [Some("critical"), None, Some("critical")]);
}

// ---------------------------------------------------------------- fixtures

/// JavaScript can hold half a surrogate pair, which a cut through an emoji leaves; Rust text cannot, so
/// a lone surrogate escape in a fixture is read as the replacement character `from_utf16_lossy` makes.
fn replace_lone_surrogates(text: &str) -> String {
    let unit = |s: &str| -> Option<u32> {
        if !s.starts_with("\\u") {
            return None;
        }
        u32::from_str_radix(s.get(2..6)?, 16).ok()
    };
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("\\u") {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match unit(rest) {
            Some(hi) if (0xD800..0xDC00).contains(&hi) && rest.get(6..).and_then(unit).is_some_and(|lo| (0xDC00..0xE000).contains(&lo)) => {
                out.push_str(&rest[..12]);
                rest = &rest[12..];
            }
            Some(u) if (0xD800..0xE000).contains(&u) => {
                out.push('\u{FFFD}');
                rest = &rest[6..];
            }
            _ => {
                out.push_str(&rest[..2]);
                rest = &rest[2..];
            }
        }
    }
    out + rest
}

fn fixture(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_str(&replace_lone_surrogates(&fs::read_to_string(path).unwrap())).unwrap()
}

fn event_of(v: &Value) -> SupervisorEvent {
    SupervisorEvent::from_value(v.clone()).expect("a fixture event")
}

#[test]
fn matches_every_describe_fixture() {
    // Arrange
    let cases = fixture("describe.json");
    // Act / Assert
    for c in &cases {
        assert_eq!(described_value(&dsc(&event_of(&c["input"]))), c["output"], "{}", c["name"]);
    }
    assert_eq!(cases.len(), 135);
}

#[test]
fn matches_every_gate_fixture() {
    // Arrange
    let cases = fixture("gate.json");
    // Act / Assert
    for c in &cases {
        let mut gate = AlertGate::default();
        let got: Vec<Value> = c["input"].as_array().unwrap().iter().map(|e| described_value(&gate.describe(&event_of(e)))).collect();
        assert_eq!(Value::Array(got), c["output"], "{}", c["name"]);
    }
    assert_eq!(cases.len(), 18);
}

#[test]
fn matches_every_playlist_names_fixture() {
    // Arrange
    let cases = fixture("playlist-names.json");
    // Act / Assert
    for c in &cases {
        let playlists: Vec<Playlist> = c["input"]["playlists"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| Playlist { id: p["id"].as_str().unwrap().into(), name: p["name"].as_str().map(String::from) })
            .collect();
        let got = with_playlist_names(event_of(&c["input"]["event"]), &playlists);
        assert_eq!(serde_json::to_value(&got).unwrap(), c["output"], "{}", c["name"]);
    }
    assert_eq!(cases.len(), 10);
}

#[test]
fn matches_every_launcher_event_fixture() {
    // Arrange
    let cases = fixture("launcher-events.json");
    // Act / Assert
    for c in &cases {
        let detail = c["input"]["detail"].as_object().unwrap().clone();
        let got = launcher_event(c["input"]["kind"].as_str().unwrap(), detail, UNIX_EPOCH + Duration::from_secs(NOON));
        let mut v = serde_json::to_value(&got).unwrap();
        v["ts"] = json!("<ts>");
        assert_eq!(v, c["output"], "{}", c["name"]);
    }
    assert_eq!(cases.len(), 10);
}

#[test]
fn launcher_texts_match_the_setup_check_constants() {
    // Arrange / Act
    let ffmpeg = dsc(&launcher_event("ffmpeg-missing", Map::new(), SystemTime::UNIX_EPOCH));
    let none = dsc(&launcher_event("no-encoder", Map::new(), SystemTime::UNIX_EPOCH));
    // Assert
    assert_eq!(var(alert(&ffmpeg), "message"), &json!(FFMPEG_MISSING));
    assert_eq!(var(alert(&none), "message"), &json!(NO_ENCODER));
}
