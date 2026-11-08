//! Port of `src/admin/status-view.js`: the texts and colour levels the dashboard shows.

use crate::api::types::{AutostartStatus, PlaylistRef, SetupReport, Stream, Twitch, Youtube};
use chrono::{DateTime, Local, Timelike};
use serde_json::{Map, Value};

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Level {
    Ok,
    Warn,
    Err,
    Muted,
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct StreamLine {
    pub word: &'static str,
    pub level: Level,
    pub reason: Option<String>,
}

fn stream_word(state: &str) -> Option<(&'static str, Level)> {
    match state {
        "live" => Some(("Live", Level::Ok)),
        "recovering" => Some(("Recovering", Level::Warn)),
        "starting" => Some(("Starting", Level::Warn)),
        "off-air" => Some(("Off air", Level::Err)),
        "stopped" => Some(("Stopped", Level::Muted)),
        _ => None,
    }
}

/// The stream line: word, colour level and the reason to show (None when Live and nothing to note).
/// `note` is `youtube_note`'s text: the slate can be live while YouTube blocks, so it shows then too.
pub fn stream_line(stream: Option<&Stream>, note: Option<&str>) -> StreamLine {
    let known = stream.and_then(|s| stream_word(&s.state));
    let (word, level) = known.unwrap_or(("Off air", Level::Err));
    let live = known.is_some() && stream.is_some_and(|s| s.state == "live");
    let own = stream.and_then(|s| s.reason.as_deref()).filter(|r| !r.is_empty());
    let reason = if live {
        None
    } else if let Some(r) = own {
        Some(r.to_string())
    } else if known.is_some() {
        None
    } else {
        Some("The server sent no stream state.".to_string())
    };
    let note = note.filter(|n| !n.is_empty());
    let stopped = known.is_some() && stream.is_some_and(|s| s.state == "stopped");
    match note {
        Some(note) if !stopped => StreamLine {
            word,
            level,
            reason: Some(match reason {
                Some(r) => format!("{r} {note}"),
                None => note.to_string(),
            }),
        },
        _ => StreamLine { word, level, reason },
    }
}

/// Why YouTube is off limits, from the supervisor's /status youtube part; None when it is not.
/// The next try is in local time, HH:MM.
pub fn youtube_note(youtube: Option<&Youtube>) -> Option<String> {
    let youtube = youtube?;
    if let Some(problem) = youtube.cookie_problem.as_deref().filter(|p| !p.is_empty()) {
        return Some(problem.to_string());
    }
    if youtube.breaker.as_deref() != Some("open") {
        return None;
    }
    let at = youtube
        .retry_at
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    match at {
        None => Some("YouTube is blocking requests.".to_string()),
        Some(at) => {
            let at = at.with_timezone(&Local);
            Some(format!("YouTube is blocking requests. Next try at {:02}:{:02}.", at.hour(), at.minute()))
        }
    }
}

/// The Twitch live check and the read-back, as one line.
pub fn twitch_line(twitch: Option<&Twitch>) -> (String, Level) {
    let Some(twitch) = twitch.filter(|t| t.enabled) else {
        return ("Twitch check off".to_string(), Level::Muted);
    };
    let (mut text, level) = match twitch.channel_live {
        None => ("Twitch: checking".to_string(), Level::Warn),
        Some(true) => ("Twitch: live".to_string(), Level::Ok),
        Some(false) => (
            "Twitch: offline".to_string(),
            if twitch.consecutive_mismatches > 0 { Level::Err } else { Level::Warn },
        ),
    };
    if twitch.consecutive_mismatches > 0 {
        text.push_str(&format!(" ({}/{})", twitch.consecutive_mismatches, twitch.mismatch_threshold));
    }

    let (state, checked) = match &twitch.readback {
        Some(rb) => (rb.state.as_str(), rb.last_check_at.is_some()),
        None => ("off", false),
    };
    match state {
        "off" => (text + " · read-back off", level),
        "frozen" => (text + " · read-back: picture frozen", Level::Err),
        "unavailable" => (
            text + " · read-back: no picture from Twitch",
            if level == Level::Ok { Level::Warn } else { level },
        ),
        _ => (
            text + if checked {
                " · read-back: picture moving"
            } else {
                " · read-back: waiting for the first check"
            },
            level,
        ),
    }
}

/// Index into the configured playlists of the list on air; the first when the supervisor names none it knows.
pub fn playing_list_index(playlists: &[PlaylistRef], playing_id: Option<&str>) -> usize {
    playlists.iter().position(|p| Some(p.id.as_str()) == playing_id).unwrap_or(0)
}

/// A refused reload is harmless: no supervisor is running, and it reads the saved config at start.
pub fn is_benign_reload_error(reload_error: Option<&str>, reload_error_code: Option<&str>) -> bool {
    reload_error.is_none_or(str::is_empty) || reload_error_code == Some("ECONNREFUSED")
}

pub fn source_label(source: Option<&str>) -> String {
    match source {
        Some("file") => "Cached file".to_string(),
        Some("url") => "YouTube".to_string(),
        Some("slate") => "Standby slate".to_string(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ => "-".to_string(),
    }
}

/// How long the wizard waits on off-air before it shows the reason, and before it gives up on anything.
pub const WIZARD_OFF_AIR_GRACE_MS: u64 = 15_000;
pub const WIZARD_TIMEOUT_MS: u64 = 90_000;

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Verdict {
    Live,
    Wait,
    Reason,
}

/// The wizard's verify step: Live ends it, Wait polls again, Reason ends it showing stream.reason.
/// Off air right after the first save is normal (the supervisor is still launching), so it gets a grace period.
pub fn wizard_verdict(stream: Option<&Stream>, elapsed_ms: u64) -> Verdict {
    let state = stream.map(|s| s.state.as_str());
    if state == Some("live") {
        return Verdict::Live;
    }
    if elapsed_ms >= WIZARD_TIMEOUT_MS {
        return Verdict::Reason;
    }
    if matches!(state, Some("starting" | "recovering")) {
        return Verdict::Wait;
    }
    if elapsed_ms >= WIZARD_OFF_AIR_GRACE_MS { Verdict::Reason } else { Verdict::Wait }
}

/// Seconds as h:mm:ss.
pub fn format_time(seconds: Option<f64>) -> String {
    let seconds = match seconds {
        Some(s) if s > 0.0 && s.is_finite() => s,
        _ => return "0:00:00".to_string(),
    };
    let h = (seconds / 3600.0).floor() as u64;
    let m = ((seconds % 3600.0) / 60.0).floor() as u64;
    let s = (seconds % 60.0).floor() as u64;
    format!("{h}:{m:02}:{s:02}")
}

/// The one list of encoder names, as (config value, shown name) in the Playback dropdown's order.
pub const ENCODER_NAMES: [(&str, &str); 4] = [
    ("h264_nvenc", "NVIDIA (NVENC)"),
    ("h264_amf", "AMD (AMF)"),
    ("h264_qsv", "Intel (Quick Sync)"),
    ("libx264", "Software (x264, uses the CPU)"),
];

/// The setup check's encoder line. works false means nothing works, so libx264 there is not a pick.
pub fn encoder_label(report: &SetupReport) -> (String, Level) {
    let enc = &report.encoder;
    if !enc.works {
        return ("No encoder works on this PC. Check the graphics driver.".to_string(), Level::Err);
    }
    let name = ENCODER_NAMES.iter().find(|(v, _)| *v == enc.chosen).map_or(enc.chosen.as_str(), |(_, n)| n);
    if enc.software {
        return (format!("{name}. No graphics card encoder works on this PC."), Level::Warn);
    }
    (name.to_string(), Level::Ok)
}

/// Where the Startup folder's StreamLoop.vbs points, from GET /api/autostart.
pub fn autostart_label(status: &AutostartStatus) -> String {
    if !status.enabled {
        return "Off".to_string();
    }
    if status.this_install {
        return "This install".to_string();
    }
    if let Some(target) = status.target.as_deref().filter(|t| !t.is_empty()) {
        return format!("Another install: {target}");
    }
    "set by another program (StreamLoop.vbs in the Startup folder)".to_string()
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct DependencyRow {
    pub ok: bool,
    pub text: String,
    pub path: String,
}

/// The dependency list with failures first, each with its display line.
pub fn dependency_rows(report: &SetupReport) -> Vec<DependencyRow> {
    let deps = &report.dependencies;
    deps.iter()
        .filter(|d| !d.ok)
        .chain(deps.iter().filter(|d| d.ok))
        .map(|d| DependencyRow {
            ok: d.ok,
            text: if d.ok {
                d.name.clone()
            } else if d.name == "Token server modules" {
                // The server's label reads badly in the missing line.
                format!("Token server packages are missing: {}", d.path)
            } else {
                format!("{} is missing: {}", d.name, d.path)
            },
            path: d.path.clone(),
        })
        .collect()
}

/// Equality as JSON.stringify sees it: 300 and 300.0 print the same, so they match.
fn same_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y || x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_json(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same_json(v, w)))
        }
        _ => a == b,
    }
}

/// The part of `edited` that differs from `original`, as a POST /api/config patch: objects key by key, arrays whole.
/// A save then never resends a value the server set on its own, such as a fallback encoder.
pub fn changed_fields(original: &Value, edited: &Value) -> Value {
    let mut out = Map::new();
    let Some(edited) = edited.as_object() else {
        return Value::Object(out);
    };
    for (key, value) in edited {
        let before = original.as_object().and_then(|o| o.get(key));
        if value.is_object() {
            let before = before.filter(|b| b.is_object()).unwrap_or(&Value::Null);
            let inner = changed_fields(before, value);
            if inner.as_object().is_some_and(|o| !o.is_empty()) {
                out.insert(key.clone(), inner);
            }
        } else if !before.is_some_and(|b| same_json(b, value)) {
            out.insert(key.clone(), value.clone());
        }
    }
    Value::Object(out)
}

/// `changed_fields` plus the user's encoder pick as the preference, which a fallback then returns to.
pub fn save_patch(original: &Value, edited: &Value) -> Value {
    let mut patch = changed_fields(original, edited);
    if let Some(sup) = patch.get_mut("supervisor").and_then(Value::as_object_mut) {
        let picked = sup.get("encoder").filter(|e| !e.is_null() && e.as_str() != Some("")).cloned();
        if let Some(encoder) = picked {
            sup.insert("preferredEncoder".to_string(), encoder);
        }
    }
    patch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Dependency, Readback, SetupCache, SetupEncoder};
    use chrono::TimeZone;
    use serde_json::json;

    fn local_rfc3339(h: u32, m: u32, s: u32) -> String {
        Local.with_ymd_and_hms(2026, 10, 26, h, m, s).unwrap().to_rfc3339()
    }

    fn stream(state: &str, reason: Option<&str>) -> Stream {
        Stream { state: state.into(), reason: reason.map(Into::into) }
    }

    fn twitch(f: impl FnOnce(&mut Twitch)) -> Twitch {
        let mut t = Twitch {
            enabled: true,
            channel_live: Some(true),
            consecutive_mismatches: 0,
            mismatch_threshold: 3,
            readback: Some(Readback { state: "ok".into(), last_check_at: Some("1".into()) }),
        };
        f(&mut t);
        t
    }

    fn report(f: impl FnOnce(&mut SetupEncoder), dependencies: Vec<Dependency>) -> SetupReport {
        let mut encoder = SetupEncoder { chosen: "h264_nvenc".into(), software: false, works: true };
        f(&mut encoder);
        SetupReport {
            at: "2026-10-25T10:00:00.000Z".into(),
            encoder,
            cache: SetupCache { free_mb: 100000, recommended_mb: 4096, warning: None },
            dependencies,
        }
    }

    fn dep(name: &str, ok: bool, path: &str) -> Dependency {
        Dependency { name: name.into(), ok, path: path.into() }
    }

    mod stream_line_tests {
        use super::*;

        #[test]
        fn shows_each_state_as_its_word_and_colour() {
            for (state, word, level) in [
                ("live", "Live", Level::Ok),
                ("recovering", "Recovering", Level::Warn),
                ("starting", "Starting", Level::Warn),
                ("off-air", "Off air", Level::Err),
                ("stopped", "Stopped", Level::Muted),
            ] {
                let line = stream_line(Some(&stream(state, Some("why"))), None);
                assert_eq!((line.word, line.level), (word, level), "{state}");
            }
        }

        #[test]
        fn hides_the_reason_while_live() {
            assert_eq!(stream_line(Some(&stream("live", Some("stale"))), None).reason, None);
        }

        #[test]
        fn shows_the_reason_when_not_live() {
            let s = stream("off-air", Some("The encoder is not running."));
            assert_eq!(stream_line(Some(&s), None).reason.as_deref(), Some("The encoder is not running."));
        }

        #[test]
        fn treats_an_unknown_or_missing_state_as_off_air_with_a_reason() {
            let weird = Stream { state: "weird".into(), reason: None };
            let line = stream_line(Some(&weird), None);
            assert_eq!((line.word, line.level), ("Off air", Level::Err));
            assert!(stream_line(None, None).reason.is_some());
        }
    }

    mod twitch_line_tests {
        use super::*;

        #[test]
        fn says_the_check_is_off_when_liveness_is_disabled() {
            let t = twitch(|t| t.enabled = false);
            assert_eq!(twitch_line(Some(&t)).0, "Twitch check off");
        }

        #[test]
        fn puts_the_read_back_next_to_the_live_check_on_one_line() {
            assert_eq!(twitch_line(Some(&twitch(|_| {}))), ("Twitch: live · read-back: picture moving".to_string(), Level::Ok));
        }

        #[test]
        fn marks_a_frozen_read_back_as_an_error_even_when_the_channel_is_live() {
            let t = twitch(|t| t.readback = Some(Readback { state: "frozen".into(), last_check_at: Some("1".into()) }));
            assert_eq!(twitch_line(Some(&t)).1, Level::Err);
        }

        #[test]
        fn shows_the_mismatch_count_while_the_channel_is_offline() {
            let t = twitch(|t| {
                t.channel_live = Some(false);
                t.consecutive_mismatches = 2;
                t.readback = Some(Readback { state: "off".into(), last_check_at: None });
            });
            assert_eq!(twitch_line(Some(&t)), ("Twitch: offline (2/3) · read-back off".to_string(), Level::Err));
        }

        #[test]
        fn says_the_read_back_is_waiting_before_its_first_check() {
            let t = twitch(|t| t.readback = Some(Readback { state: "ok".into(), last_check_at: None }));
            assert!(twitch_line(Some(&t)).0.contains("waiting for the first check"));
        }
    }

    mod is_benign_reload_error_tests {
        use super::*;

        #[test]
        fn treats_no_error_and_a_refused_connection_as_benign() {
            assert!(is_benign_reload_error(None, None));
            assert!(is_benign_reload_error(Some("fetch failed"), Some("ECONNREFUSED")));
        }

        #[test]
        fn reports_any_other_failed_fetch_such_as_a_timeout_or_a_reset() {
            assert!(!is_benign_reload_error(Some("fetch failed"), Some("ECONNRESET")));
            assert!(!is_benign_reload_error(Some("The operation was aborted due to timeout"), None));
        }

        #[test]
        fn reports_an_error_the_supervisor_answered_with() {
            assert!(!is_benign_reload_error(Some("HTTP 400"), None));
        }
    }

    mod source_label_tests {
        use super::*;

        #[test]
        fn names_the_known_sources_and_passes_others_through() {
            assert_eq!(source_label(Some("slate")), "Standby slate");
            assert_eq!(source_label(Some("other")), "other");
            assert_eq!(source_label(None), "-");
        }
    }

    mod wizard_verdict_tests {
        use super::*;

        #[test]
        fn ends_on_live() {
            assert_eq!(wizard_verdict(Some(&stream("live", None)), 0), Verdict::Live);
        }

        #[test]
        fn waits_through_starting_until_the_timeout() {
            let s = stream("starting", None);
            assert_eq!(wizard_verdict(Some(&s), WIZARD_OFF_AIR_GRACE_MS + 1), Verdict::Wait);
            assert_eq!(wizard_verdict(Some(&s), WIZARD_TIMEOUT_MS), Verdict::Reason);
        }

        #[test]
        fn gives_off_air_a_grace_period_before_showing_the_reason() {
            let s = stream("off-air", None);
            assert_eq!(wizard_verdict(Some(&s), 1000), Verdict::Wait);
            assert_eq!(wizard_verdict(Some(&s), WIZARD_OFF_AIR_GRACE_MS), Verdict::Reason);
        }
    }

    mod format_time_tests {
        use super::*;

        #[test]
        fn formats_seconds_as_h_mm_ss() {
            assert_eq!(format_time(Some(3725.9)), "1:02:05");
            assert_eq!(format_time(None), "0:00:00");
        }
    }

    mod encoder_label_tests {
        use super::*;

        #[test]
        fn names_a_working_hardware_encoder_as_ok() {
            assert_eq!(encoder_label(&report(|_| {}, vec![])), ("NVIDIA (NVENC)".to_string(), Level::Ok));
        }

        #[test]
        fn warns_when_only_the_software_encoder_works() {
            let label = encoder_label(&report(
                |e| {
                    e.chosen = "libx264".into();
                    e.software = true;
                },
                vec![],
            ));
            assert_eq!(label.1, Level::Warn);
            assert!(label.0.contains("Software"));
        }

        #[test]
        fn shows_an_error_not_a_software_pick_when_no_encoder_works() {
            let label = encoder_label(&report(
                |e| {
                    e.chosen = "libx264".into();
                    e.software = true;
                    e.works = false;
                },
                vec![],
            ));
            assert_eq!(label, ("No encoder works on this PC. Check the graphics driver.".to_string(), Level::Err));
        }
    }

    mod autostart_label_tests {
        use super::*;

        fn status(enabled: bool, target: Option<&str>, this_install: bool) -> AutostartStatus {
            AutostartStatus { enabled, target: target.map(Into::into), this_install }
        }

        #[test]
        fn says_off_when_there_is_no_autostart_file() {
            assert_eq!(autostart_label(&status(false, None, false)), "Off");
        }

        #[test]
        fn says_this_install_when_the_file_starts_this_install() {
            assert_eq!(autostart_label(&status(true, Some("C:\\SL\\START.bat"), true)), "This install");
        }

        #[test]
        fn names_the_other_install_the_file_starts() {
            assert_eq!(
                autostart_label(&status(true, Some("D:\\Old\\START.bat"), false)),
                "Another install: D:\\Old\\START.bat"
            );
        }

        #[test]
        fn flags_a_streamloop_vbs_that_streamloop_did_not_write() {
            assert_eq!(
                autostart_label(&status(true, None, false)),
                "set by another program (StreamLoop.vbs in the Startup folder)"
            );
        }
    }

    mod dependency_rows_tests {
        use super::*;

        #[test]
        fn lists_failures_first_and_keeps_the_order_otherwise() {
            let rows = dependency_rows(&report(
                |_| {},
                vec![dep("Supervisor", true, "a"), dep("ffmpeg", false, "b"), dep("yt-dlp", true, "c"), dep("Deno", false, "d")],
            ));
            let paths: Vec<_> = rows.iter().map(|r| r.path.as_str()).collect();
            assert_eq!(paths, ["b", "d", "a", "c"]);
        }

        #[test]
        fn gives_each_row_a_line_that_reads_as_a_sentence() {
            let rows = dependency_rows(&report(
                |_| {},
                vec![
                    dep("Token server modules", false, "C:\\SL\\pot\\node_modules"),
                    dep("ffmpeg", false, "C:\\SL\\ffmpeg\\ffmpeg.exe"),
                    dep("Deno", true, "C:\\SL\\yt-dlp\\deno.exe"),
                ],
            ));
            let texts: Vec<_> = rows.iter().map(|r| r.text.as_str()).collect();
            assert_eq!(
                texts,
                [
                    "Token server packages are missing: C:\\SL\\pot\\node_modules",
                    "ffmpeg is missing: C:\\SL\\ffmpeg\\ffmpeg.exe",
                    "Deno",
                ]
            );
        }
    }

    mod save_patch_tests {
        use super::*;

        fn loaded() -> Value {
            json!({ "supervisor": { "encoder": "h264_nvenc", "cacheBudgetMb": 300 } })
        }

        #[test]
        fn makes_a_picked_encoder_the_preference() {
            let edited = json!({ "supervisor": { "encoder": "libx264", "cacheBudgetMb": 300 } });
            assert_eq!(
                save_patch(&loaded(), &edited),
                json!({ "supervisor": { "encoder": "libx264", "preferredEncoder": "libx264" } })
            );
        }

        #[test]
        fn sends_no_preference_when_the_encoder_was_not_changed() {
            let edited = json!({ "supervisor": { "encoder": "h264_nvenc", "cacheBudgetMb": 500 } });
            assert_eq!(save_patch(&loaded(), &edited), json!({ "supervisor": { "cacheBudgetMb": 500 } }));
        }
    }

    mod changed_fields_tests {
        use super::*;

        fn loaded() -> Value {
            json!({
                "playlists": [{ "id": "PL1" }],
                "twitchClientSecret": "********",
                "supervisor": { "encoder": "libx264", "cacheBudgetMb": 300, "overlay": true },
            })
        }

        #[test]
        fn leaves_out_a_fallback_encoder_the_user_did_not_pick() {
            let edited = json!({ "supervisor": { "encoder": "libx264", "cacheBudgetMb": 500, "overlay": true } });
            assert_eq!(changed_fields(&loaded(), &edited), json!({ "supervisor": { "cacheBudgetMb": 500 } }));
        }

        #[test]
        fn sends_the_encoder_when_the_user_picked_another_one() {
            let edited = json!({ "supervisor": { "encoder": "h264_nvenc" } });
            assert_eq!(changed_fields(&loaded(), &edited), json!({ "supervisor": { "encoder": "h264_nvenc" } }));
        }

        #[test]
        fn returns_nothing_when_nothing_changed_including_the_masked_secret() {
            let edited = json!({ "playlists": [{ "id": "PL1" }], "twitchClientSecret": "********", "supervisor": { "overlay": true } });
            assert_eq!(changed_fields(&loaded(), &edited), json!({}));
        }

        #[test]
        fn sends_a_changed_array_whole() {
            let edited = json!({ "playlists": [{ "id": "PL2" }, { "id": "PL1" }] });
            assert_eq!(changed_fields(&loaded(), &edited), edited);
        }

        #[test]
        fn treats_an_integral_float_as_the_same_number() {
            let original = json!({ "supervisor": { "cacheBudgetMb": 300 } });
            let edited = json!({ "supervisor": { "cacheBudgetMb": 300.0 } });
            assert_eq!(changed_fields(&original, &edited), json!({}));
            let in_array = changed_fields(&json!({ "xs": [1, 2] }), &json!({ "xs": [1.0, 2.0] }));
            assert_eq!(in_array, json!({}));
        }

        #[test]
        fn sends_a_real_number_change() {
            let original = json!({ "supervisor": { "cacheBudgetMb": 300 } });
            let edited = json!({ "supervisor": { "cacheBudgetMb": 300.5 } });
            assert_eq!(changed_fields(&original, &edited), edited);
        }

        #[test]
        fn sends_a_field_the_loaded_config_did_not_have() {
            let edited = json!({ "supervisor": { "cacheBudgetMb": 50 } });
            assert_eq!(changed_fields(&json!({}), &edited), edited);
        }
    }

    mod encoder_names_tests {
        use super::*;

        #[test]
        fn encoder_names_are_in_dropdown_order() {
            assert_eq!(
                ENCODER_NAMES,
                [
                    ("h264_nvenc", "NVIDIA (NVENC)"),
                    ("h264_amf", "AMD (AMF)"),
                    ("h264_qsv", "Intel (Quick Sync)"),
                    ("libx264", "Software (x264, uses the CPU)"),
                ]
            );
        }
    }

    mod youtube_note_tests {
        use super::*;

        fn yt(breaker: &str, retry_at: Option<String>, cookie: Option<&str>) -> Youtube {
            Youtube { breaker: Some(breaker.into()), retry_at, cookie_problem: cookie.map(Into::into) }
        }

        #[test]
        fn gives_the_next_try_in_local_time_while_the_breaker_is_open() {
            let y = yt("open", Some(local_rfc3339(10, 41, 30)), None);
            assert_eq!(youtube_note(Some(&y)).as_deref(), Some("YouTube is blocking requests. Next try at 10:41."));
        }

        #[test]
        fn pads_single_digit_hours_and_minutes() {
            let y = yt("open", Some(local_rfc3339(7, 5, 0)), None);
            assert_eq!(youtube_note(Some(&y)).as_deref(), Some("YouTube is blocking requests. Next try at 07:05."));
        }

        #[test]
        fn leaves_out_the_time_when_the_supervisor_gives_none() {
            let y = yt("open", None, None);
            assert_eq!(youtube_note(Some(&y)).as_deref(), Some("YouTube is blocking requests."));
        }

        #[test]
        fn says_nothing_while_the_breaker_is_closed_or_the_status_has_no_youtube_part() {
            assert_eq!(youtube_note(Some(&yt("closed", None, None))), None);
            assert_eq!(youtube_note(None), None);
        }

        #[test]
        fn names_the_cookie_problem_which_is_not_youtube_blocking() {
            let y = yt("closed", None, Some("yt-dlp could not read the firefox cookies: locked"));
            assert_eq!(youtube_note(Some(&y)).as_deref(), Some("yt-dlp could not read the firefox cookies: locked"));
        }
    }

    mod stream_line_with_a_youtube_note {
        use super::*;

        #[test]
        fn adds_the_note_to_the_reason_even_while_live_on_the_slate() {
            let note = "YouTube is blocking requests.";
            assert_eq!(stream_line(Some(&stream("live", None)), Some(note)).reason.as_deref(), Some(note));
            let s = stream("recovering", Some("Recovering the current video."));
            assert_eq!(
                stream_line(Some(&s), Some(note)).reason.as_deref(),
                Some("Recovering the current video. YouTube is blocking requests.")
            );
        }

        #[test]
        fn shows_no_note_while_the_stream_is_stopped() {
            let s = stream("stopped", Some("Stopped."));
            assert_eq!(stream_line(Some(&s), Some("YouTube is blocking requests.")).reason.as_deref(), Some("Stopped."));
        }
    }

    mod playing_list_index_tests {
        use super::*;

        fn lists() -> Vec<PlaylistRef> {
            vec![PlaylistRef { id: "PLa".into(), name: None }, PlaylistRef { id: "PLb".into(), name: None }]
        }

        #[test]
        fn finds_the_list_on_air_in_the_configured_order() {
            assert_eq!(playing_list_index(&lists(), Some("PLb")), 1);
        }

        #[test]
        fn takes_the_first_list_when_the_supervisor_names_none() {
            assert_eq!(playing_list_index(&lists(), None), 0);
        }

        #[test]
        fn takes_the_first_list_when_the_one_on_air_has_left_the_config() {
            assert_eq!(playing_list_index(&lists(), Some("PLgone")), 0);
        }
    }
}
