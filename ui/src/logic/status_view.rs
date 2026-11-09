//! Port of `src/admin/status-view.js`: the texts and colour levels the dashboard shows.

use super::update_text::{failure_text, step_label};
use crate::api::types::{
    AutostartStatus, PlaylistRef, SetupReport, Stream, SwapLine, Twitch, UpdateProgress,
    UpdateStatus, Youtube,
};
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
    let own = stream
        .and_then(|s| s.reason.as_deref())
        .filter(|r| !r.is_empty());
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
        _ => StreamLine {
            word,
            level,
            reason,
        },
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
            Some(format!(
                "YouTube is blocking requests. Next try at {:02}:{:02}.",
                at.hour(),
                at.minute()
            ))
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
            if twitch.consecutive_mismatches > 0 {
                Level::Err
            } else {
                Level::Warn
            },
        ),
    };
    if twitch.consecutive_mismatches > 0 {
        text.push_str(&format!(
            " ({}/{})",
            twitch.consecutive_mismatches, twitch.mismatch_threshold
        ));
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
            if level == Level::Ok {
                Level::Warn
            } else {
                level
            },
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
    playlists
        .iter()
        .position(|p| Some(p.id.as_str()) == playing_id)
        .unwrap_or(0)
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
    if elapsed_ms >= WIZARD_OFF_AIR_GRACE_MS {
        Verdict::Reason
    } else {
        Verdict::Wait
    }
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
        return (
            "No encoder works on this PC. Check the graphics driver.".to_string(),
            Level::Err,
        );
    }
    let name = ENCODER_NAMES
        .iter()
        .find(|(v, _)| *v == enc.chosen)
        .map_or(enc.chosen.as_str(), |(_, n)| n);
    if enc.software {
        return (
            format!("{name}. No graphics card encoder works on this PC."),
            Level::Warn,
        );
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
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_json(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_json(v, w)))
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
        let picked = sup
            .get("encoder")
            .filter(|e| !e.is_null() && e.as_str() != Some(""))
            .cloned();
        if let Some(encoder) = picked {
            sup.insert("preferredEncoder".to_string(), encoder);
        }
    }
    patch
}

/// "45 s", "1 min 20 s", "12 min": seconds only under 10 minutes.
pub fn format_elapsed(ms: i64) -> String {
    let total = ms.div_euclid(1000).max(0);
    let (min, sec) = (total / 60, total % 60);
    if min == 0 {
        format!("{sec} s")
    } else if min >= 10 || sec == 0 {
        format!("{min} min")
    } else {
        format!("{min} min {sec} s")
    }
}

const PART_LABELS: [(&str, &str); 5] = [
    ("node", "Node"),
    ("supervisor", "Supervisor"),
    ("ffmpeg", "ffmpeg"),
    ("pot", "Token server"),
    ("ytdlp-plugin", "yt-dlp plugin"),
];

/// A number as JavaScript prints it in a template string: `42`, not `42.0`.
fn js_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The offer: the full update's banner, the same-version button copy, or the app-only text.
pub fn update_offer_text(status: &UpdateStatus) -> String {
    let latest = status.latest_version.as_deref().unwrap_or("");
    if status.kind.as_deref() == Some("full") {
        if status.same_version {
            return format!("Update parts to {latest}");
        }
        let names: Vec<&str> = status
            .parts
            .iter()
            .map(|p| {
                PART_LABELS
                    .iter()
                    .find(|(key, _)| key == p)
                    .map_or(p.as_str(), |(_, label)| label)
            })
            .collect();
        return format!(
            "Full update: the stream goes off air while these parts are replaced: {}.",
            names.join(", ")
        );
    }
    format!(
        "Version {latest} is available (current: {})",
        status.current_version.as_deref().unwrap_or("")
    )
}

/// The banner while an app-only update downloads; a full update shows `update_progress_text` instead.
pub fn app_download_text(status: &UpdateStatus) -> String {
    let percent = status
        .download_percent
        .map(|p| format!(", {}%", js_number(p)))
        .unwrap_or_default();
    format!(
        "Downloading v{}{percent}...",
        status.latest_version.as_deref().unwrap_or("")
    )
}

/// True while a full update owns the stream: the dashboard shows its progress instead of the stream status.
pub fn update_running(progress: Option<&UpdateProgress>) -> bool {
    progress.is_some_and(|p| p.result == "running")
}

/// `Date.parse` of an ISO time as milliseconds since the epoch; None is JS's NaN.
fn parse_ms(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// The progress line: the step with its elapsed time while running, else the result.
pub fn update_progress_text(p: &UpdateProgress, swap_log: &[SwapLine], now_ms: i64) -> String {
    if p.result == "ok" {
        return if p.step == "done" {
            format!("Back on air on {}.", p.to)
        } else {
            format!("Updated to {}. Ready to stream.", p.to)
        };
    }
    if p.result != "running" {
        return failure_text(p);
    }
    let percent = match p.download_percent {
        Some(n) if p.step == "download" => format!("{}%, ", js_number(n)),
        _ => String::new(),
    };
    let elapsed = parse_ms(&p.step_started_at).map_or(0, |since| now_ms - since);
    let mut text = format!(
        "{}, {percent}{}",
        step_label(&p.step),
        format_elapsed(elapsed)
    );
    if let Some(last) = swap_log
        .last()
        .filter(|l| p.step == "swap" && l.status == "retry")
    {
        text.push_str(&format!(" (trying {} again)", last.folder));
    }
    text
}

const OK_BANNER_MS: i64 = 10 * 60 * 1000;

#[derive(PartialEq, Debug, Clone)]
pub struct BannerView {
    pub visible: bool,
    pub text: String,
    pub show_button: bool,
    /// The text is the update's own (progress or result), not the offer's.
    pub keep_text: bool,
}

/// What the update banner shows. A failed result keeps its text beside the offer; ok gives way to
/// it after 10 minutes, and "Ready" goes once `stream_state` is live.
pub fn update_banner_view(
    status: &UpdateStatus,
    now_ms: i64,
    stream_state: Option<&str>,
) -> BannerView {
    if let Some(p) = &status.progress {
        let running = p.result == "running";
        let current = status.current_version.as_deref();
        // From and to other versions than the running one: a later update replaced it.
        let history =
            !running && Some(p.from.as_str()) != current && Some(p.to.as_str()) != current;
        let ready_while_live = p.result == "ok" && p.step != "done" && stream_state == Some("live");
        if !history && !ready_while_live {
            let text = update_progress_text(p, &status.swap_log, now_ms);
            let shown = |show_button| BannerView {
                visible: true,
                text: text.clone(),
                show_button,
                keep_text: true,
            };
            if running {
                return shown(false);
            }
            if p.result == "failed" || p.result == "rolled-back" {
                return shown(status.update_available);
            }
            let fresh =
                parse_ms(&p.step_started_at).is_some_and(|since| now_ms - since < OK_BANNER_MS);
            if p.result == "ok" && fresh {
                return shown(false);
            }
        }
    }
    BannerView {
        visible: status.update_available,
        text: String::new(),
        show_button: true,
        keep_text: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{Dependency, Readback, SetupCache, SetupEncoder};
    use chrono::TimeZone;
    use serde_json::json;

    fn local_rfc3339(h: u32, m: u32, s: u32) -> String {
        Local
            .with_ymd_and_hms(2026, 10, 26, h, m, s)
            .unwrap()
            .to_rfc3339()
    }

    fn stream(state: &str, reason: Option<&str>) -> Stream {
        Stream {
            state: state.into(),
            reason: reason.map(Into::into),
        }
    }

    fn twitch(f: impl FnOnce(&mut Twitch)) -> Twitch {
        let mut t = Twitch {
            enabled: true,
            channel_live: Some(true),
            consecutive_mismatches: 0,
            mismatch_threshold: 3,
            readback: Some(Readback {
                state: "ok".into(),
                last_check_at: Some("1".into()),
            }),
        };
        f(&mut t);
        t
    }

    fn report(f: impl FnOnce(&mut SetupEncoder), dependencies: Vec<Dependency>) -> SetupReport {
        let mut encoder = SetupEncoder {
            chosen: "h264_nvenc".into(),
            software: false,
            works: true,
        };
        f(&mut encoder);
        SetupReport {
            at: "2026-10-25T10:00:00.000Z".into(),
            encoder,
            cache: SetupCache {
                free_mb: 100000,
                recommended_mb: 4096,
                warning: None,
            },
            dependencies,
        }
    }

    fn dep(name: &str, ok: bool, path: &str) -> Dependency {
        Dependency {
            name: name.into(),
            ok,
            path: path.into(),
        }
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
            assert_eq!(
                stream_line(Some(&stream("live", Some("stale"))), None).reason,
                None
            );
        }

        #[test]
        fn shows_the_reason_when_not_live() {
            let s = stream("off-air", Some("The encoder is not running."));
            assert_eq!(
                stream_line(Some(&s), None).reason.as_deref(),
                Some("The encoder is not running.")
            );
        }

        #[test]
        fn treats_an_unknown_or_missing_state_as_off_air_with_a_reason() {
            let weird = Stream {
                state: "weird".into(),
                reason: None,
            };
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
            assert_eq!(
                twitch_line(Some(&twitch(|_| {}))),
                (
                    "Twitch: live · read-back: picture moving".to_string(),
                    Level::Ok
                )
            );
        }

        #[test]
        fn marks_a_frozen_read_back_as_an_error_even_when_the_channel_is_live() {
            let t = twitch(|t| {
                t.readback = Some(Readback {
                    state: "frozen".into(),
                    last_check_at: Some("1".into()),
                })
            });
            assert_eq!(twitch_line(Some(&t)).1, Level::Err);
        }

        #[test]
        fn shows_the_mismatch_count_while_the_channel_is_offline() {
            let t = twitch(|t| {
                t.channel_live = Some(false);
                t.consecutive_mismatches = 2;
                t.readback = Some(Readback {
                    state: "off".into(),
                    last_check_at: None,
                });
            });
            assert_eq!(
                twitch_line(Some(&t)),
                (
                    "Twitch: offline (2/3) · read-back off".to_string(),
                    Level::Err
                )
            );
        }

        #[test]
        fn says_the_read_back_is_waiting_before_its_first_check() {
            let t = twitch(|t| {
                t.readback = Some(Readback {
                    state: "ok".into(),
                    last_check_at: None,
                })
            });
            assert!(
                twitch_line(Some(&t))
                    .0
                    .contains("waiting for the first check")
            );
        }
    }

    mod is_benign_reload_error_tests {
        use super::*;

        #[test]
        fn treats_no_error_and_a_refused_connection_as_benign() {
            assert!(is_benign_reload_error(None, None));
            assert!(is_benign_reload_error(
                Some("fetch failed"),
                Some("ECONNREFUSED")
            ));
        }

        #[test]
        fn reports_any_other_failed_fetch_such_as_a_timeout_or_a_reset() {
            assert!(!is_benign_reload_error(
                Some("fetch failed"),
                Some("ECONNRESET")
            ));
            assert!(!is_benign_reload_error(
                Some("The operation was aborted due to timeout"),
                None
            ));
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
            assert_eq!(
                wizard_verdict(Some(&stream("live", None)), 0),
                Verdict::Live
            );
        }

        #[test]
        fn waits_through_starting_until_the_timeout() {
            let s = stream("starting", None);
            assert_eq!(
                wizard_verdict(Some(&s), WIZARD_OFF_AIR_GRACE_MS + 1),
                Verdict::Wait
            );
            assert_eq!(wizard_verdict(Some(&s), WIZARD_TIMEOUT_MS), Verdict::Reason);
        }

        #[test]
        fn gives_off_air_a_grace_period_before_showing_the_reason() {
            let s = stream("off-air", None);
            assert_eq!(wizard_verdict(Some(&s), 1000), Verdict::Wait);
            assert_eq!(
                wizard_verdict(Some(&s), WIZARD_OFF_AIR_GRACE_MS),
                Verdict::Reason
            );
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
            assert_eq!(
                encoder_label(&report(|_| {}, vec![])),
                ("NVIDIA (NVENC)".to_string(), Level::Ok)
            );
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
            assert_eq!(
                label,
                (
                    "No encoder works on this PC. Check the graphics driver.".to_string(),
                    Level::Err
                )
            );
        }
    }

    mod autostart_label_tests {
        use super::*;

        fn status(enabled: bool, target: Option<&str>, this_install: bool) -> AutostartStatus {
            AutostartStatus {
                enabled,
                target: target.map(Into::into),
                this_install,
            }
        }

        #[test]
        fn says_off_when_there_is_no_autostart_file() {
            assert_eq!(autostart_label(&status(false, None, false)), "Off");
        }

        #[test]
        fn says_this_install_when_the_file_starts_this_install() {
            assert_eq!(
                autostart_label(&status(true, Some("C:\\SL\\START.bat"), true)),
                "This install"
            );
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
                vec![
                    dep("Supervisor", true, "a"),
                    dep("ffmpeg", false, "b"),
                    dep("yt-dlp", true, "c"),
                    dep("Deno", false, "d"),
                ],
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
            assert_eq!(
                save_patch(&loaded(), &edited),
                json!({ "supervisor": { "cacheBudgetMb": 500 } })
            );
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
            assert_eq!(
                changed_fields(&loaded(), &edited),
                json!({ "supervisor": { "cacheBudgetMb": 500 } })
            );
        }

        #[test]
        fn sends_the_encoder_when_the_user_picked_another_one() {
            let edited = json!({ "supervisor": { "encoder": "h264_nvenc" } });
            assert_eq!(
                changed_fields(&loaded(), &edited),
                json!({ "supervisor": { "encoder": "h264_nvenc" } })
            );
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
            Youtube {
                breaker: Some(breaker.into()),
                retry_at,
                cookie_problem: cookie.map(Into::into),
            }
        }

        #[test]
        fn gives_the_next_try_in_local_time_while_the_breaker_is_open() {
            let y = yt("open", Some(local_rfc3339(10, 41, 30)), None);
            assert_eq!(
                youtube_note(Some(&y)).as_deref(),
                Some("YouTube is blocking requests. Next try at 10:41.")
            );
        }

        #[test]
        fn pads_single_digit_hours_and_minutes() {
            let y = yt("open", Some(local_rfc3339(7, 5, 0)), None);
            assert_eq!(
                youtube_note(Some(&y)).as_deref(),
                Some("YouTube is blocking requests. Next try at 07:05.")
            );
        }

        #[test]
        fn leaves_out_the_time_when_the_supervisor_gives_none() {
            let y = yt("open", None, None);
            assert_eq!(
                youtube_note(Some(&y)).as_deref(),
                Some("YouTube is blocking requests.")
            );
        }

        #[test]
        fn says_nothing_while_the_breaker_is_closed_or_the_status_has_no_youtube_part() {
            assert_eq!(youtube_note(Some(&yt("closed", None, None))), None);
            assert_eq!(youtube_note(None), None);
        }

        #[test]
        fn names_the_cookie_problem_which_is_not_youtube_blocking() {
            let y = yt(
                "closed",
                None,
                Some("yt-dlp could not read the firefox cookies: locked"),
            );
            assert_eq!(
                youtube_note(Some(&y)).as_deref(),
                Some("yt-dlp could not read the firefox cookies: locked")
            );
        }
    }

    mod stream_line_with_a_youtube_note {
        use super::*;

        #[test]
        fn adds_the_note_to_the_reason_even_while_live_on_the_slate() {
            let note = "YouTube is blocking requests.";
            assert_eq!(
                stream_line(Some(&stream("live", None)), Some(note))
                    .reason
                    .as_deref(),
                Some(note)
            );
            let s = stream("recovering", Some("Recovering the current video."));
            assert_eq!(
                stream_line(Some(&s), Some(note)).reason.as_deref(),
                Some("Recovering the current video. YouTube is blocking requests.")
            );
        }

        #[test]
        fn shows_no_note_while_the_stream_is_stopped() {
            let s = stream("stopped", Some("Stopped."));
            assert_eq!(
                stream_line(Some(&s), Some("YouTube is blocking requests."))
                    .reason
                    .as_deref(),
                Some("Stopped.")
            );
        }
    }

    mod playing_list_index_tests {
        use super::*;

        fn lists() -> Vec<PlaylistRef> {
            vec![
                PlaylistRef {
                    id: "PLa".into(),
                    name: None,
                },
                PlaylistRef {
                    id: "PLb".into(),
                    name: None,
                },
            ]
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

    mod full_update {
        use super::*;
        use crate::api::types::{SwapLine, UpdateProgress, UpdateStatus};

        const T0_ISO: &str = "2026-11-08T10:00:00.000Z";

        fn t0() -> i64 {
            DateTime::parse_from_rfc3339(T0_ISO)
                .unwrap()
                .timestamp_millis()
        }

        fn at(s: i64) -> i64 {
            t0() + s * 1000
        }

        fn state(over: impl FnOnce(&mut UpdateProgress)) -> UpdateProgress {
            let mut p = UpdateProgress {
                from: "3.0.5".into(),
                to: "3.0.6".into(),
                parts: vec!["node".into()],
                folders: vec!["node".into()],
                step: "swap".into(),
                result: "running".into(),
                reason: None,
                started_at: T0_ISO.into(),
                step_started_at: T0_ISO.into(),
                download_percent: None,
            };
            over(&mut p);
            p
        }

        #[test]
        fn formats_elapsed_times() {
            assert_eq!(format_elapsed(0), "0 s");
            assert_eq!(format_elapsed(45_000), "45 s");
            assert_eq!(format_elapsed(80_000), "1 min 20 s");
            assert_eq!(format_elapsed(12 * 60_000 + 5_000), "12 min");
            assert_eq!(format_elapsed(-5), "0 s");
        }

        #[test]
        fn floors_elapsed_milliseconds() {
            assert_eq!(format_elapsed(999), "0 s");
            assert_eq!(format_elapsed(-1500), "0 s");
            assert_eq!(format_elapsed(1999), "1 s");
        }

        fn offer_base() -> UpdateStatus {
            UpdateStatus {
                current_version: Some("3.0.5".into()),
                latest_version: Some("3.0.6".into()),
                kind: Some("app".into()),
                ..Default::default()
            }
        }

        #[test]
        fn offer_text_keeps_the_app_only_text() {
            assert_eq!(
                update_offer_text(&offer_base()),
                "Version 3.0.6 is available (current: 3.0.5)"
            );
        }

        #[test]
        fn offer_text_names_the_parts_of_a_full_update() {
            let status = UpdateStatus {
                kind: Some("full".into()),
                parts: vec!["node".into(), "ffmpeg".into(), "ytdlp-plugin".into()],
                ..offer_base()
            };

            assert_eq!(
                update_offer_text(&status),
                "Full update: the stream goes off air while these parts are replaced: Node, ffmpeg, yt-dlp plugin."
            );
        }

        #[test]
        fn offer_text_offers_a_same_version_parts_update() {
            let status = UpdateStatus {
                latest_version: Some("3.0.5".into()),
                kind: Some("full".into()),
                parts: vec!["pot".into()],
                same_version: true,
                ..offer_base()
            };

            assert_eq!(update_offer_text(&status), "Update parts to 3.0.5");
        }

        #[test]
        fn offer_text_prints_an_unknown_part_as_itself() {
            let status = UpdateStatus {
                kind: Some("full".into()),
                parts: vec!["supervisor".into(), "mystery".into()],
                ..offer_base()
            };

            assert_eq!(
                update_offer_text(&status),
                "Full update: the stream goes off air while these parts are replaced: Supervisor, mystery."
            );
        }

        #[test]
        fn progress_text_shows_the_step_and_its_elapsed_time() {
            assert_eq!(
                update_progress_text(&state(|_| {}), &[], at(80)),
                "Replacing files, 1 min 20 s"
            );
            assert_eq!(
                update_progress_text(&state(|p| p.step = "stop".into()), &[], at(5)),
                "Stopping the stream, 5 s"
            );
            assert_eq!(
                update_progress_text(&state(|p| p.step = "download".into()), &[], at(0)),
                "Downloading, 0 s"
            );
            assert_eq!(
                update_progress_text(&state(|p| p.step = "check".into()), &[], at(3)),
                "Checking the new version, 3 s"
            );
            assert_eq!(
                update_progress_text(&state(|p| p.step = "start".into()), &[], at(3)),
                "Starting the stream, 3 s"
            );
        }

        #[test]
        fn progress_text_shows_the_download_percentage_only_while_downloading() {
            let download = |percent| {
                state(|p| {
                    p.step = "download".into();
                    p.download_percent = percent;
                })
            };

            assert_eq!(
                update_progress_text(&download(Some(42.0)), &[], at(70)),
                "Downloading, 42%, 1 min 10 s"
            );
            assert_eq!(
                update_progress_text(&download(None), &[], at(70)),
                "Downloading, 1 min 10 s"
            );
            assert_eq!(
                update_progress_text(&download(Some(42.5)), &[], at(70)),
                "Downloading, 42.5%, 1 min 10 s"
            );
            let stop = state(|p| {
                p.step = "stop".into();
                p.download_percent = Some(100.0);
            });
            assert_eq!(
                update_progress_text(&stop, &[], at(5)),
                "Stopping the stream, 5 s"
            );
        }

        #[test]
        fn progress_text_mentions_a_swap_retry() {
            let log = [SwapLine {
                folder: "node".into(),
                status: "retry".into(),
                detail: "in use".into(),
            }];

            assert_eq!(
                update_progress_text(&state(|_| {}), &log, at(10)),
                "Replacing files, 10 s (trying node again)"
            );
        }

        #[test]
        fn progress_text_ignores_a_retry_that_is_not_the_last_line_or_not_the_swap() {
            let retry = SwapLine {
                folder: "node".into(),
                status: "retry".into(),
                detail: String::new(),
            };
            let done = SwapLine {
                folder: "node".into(),
                status: "ok".into(),
                detail: String::new(),
            };

            assert_eq!(
                update_progress_text(&state(|_| {}), &[retry.clone(), done], at(10)),
                "Replacing files, 10 s"
            );
            assert_eq!(
                update_progress_text(&state(|p| p.step = "stop".into()), &[retry], at(10)),
                "Stopping the stream, 10 s"
            );
        }

        #[test]
        fn progress_text_counts_zero_elapsed_when_the_step_time_does_not_parse() {
            let broken = state(|p| p.step_started_at = "not a date".into());

            assert_eq!(
                update_progress_text(&broken, &[], at(80)),
                "Replacing files, 0 s"
            );
        }

        #[test]
        fn progress_text_shows_the_ready_copy_when_the_update_went_through_and_back_on_air_once_the_stream_is_up()
         {
            let ok = |step: &str| {
                state(|p| {
                    p.step = step.into();
                    p.result = "ok".into();
                })
            };

            assert_eq!(
                update_progress_text(&ok("start"), &[], at(1)),
                "Updated to 3.0.6. Ready to stream."
            );
            assert_eq!(
                update_progress_text(&ok("done"), &[], at(1)),
                "Back on air on 3.0.6."
            );
        }

        #[test]
        fn progress_text_shows_the_failure_with_step_and_reason() {
            let failed = state(|p| {
                p.result = "failed".into();
                p.reason = Some("ffmpeg is still running.".into());
            });
            let back = state(|p| {
                p.step = "check".into();
                p.result = "rolled-back".into();
                p.reason = Some("No encoder works.".into());
            });

            assert_eq!(
                update_progress_text(&failed, &[], at(1)),
                "The update to 3.0.6 failed at replacing files: ffmpeg is still running. Still on 3.0.5."
            );
            assert_eq!(
                update_progress_text(&back, &[], at(1)),
                "The update to 3.0.6 failed at checking the new version: No encoder works. Still on 3.0.5."
            );
        }

        #[test]
        fn failure_text_names_a_missing_reason() {
            let failed = state(|p| p.result = "failed".into());

            assert_eq!(
                failure_text(&failed),
                "The update to 3.0.6 failed at replacing files: no reason was recorded. Still on 3.0.5."
            );
        }

        #[test]
        fn step_label_prints_an_unknown_step_as_itself() {
            assert_eq!(step_label("done"), "Back on air");
            assert_eq!(step_label("warp"), "warp");
        }

        #[test]
        fn download_text_adds_the_percentage_of_an_app_only_download_when_the_server_knows_it() {
            let status = |percent| UpdateStatus {
                latest_version: Some("3.0.7".into()),
                download_percent: percent,
                ..Default::default()
            };

            assert_eq!(
                app_download_text(&status(Some(42.0))),
                "Downloading v3.0.7, 42%..."
            );
            assert_eq!(app_download_text(&status(None)), "Downloading v3.0.7...");
        }

        #[test]
        fn update_running_is_true_only_while_the_result_is_running() {
            assert!(update_running(Some(&state(|_| {}))));
            assert!(!update_running(Some(&state(|p| p.result = "ok".into()))));
            assert!(!update_running(None));
        }

        fn status(over: impl FnOnce(&mut UpdateStatus)) -> UpdateStatus {
            let mut s = UpdateStatus {
                current_version: Some("3.0.6".into()),
                status: "idle".into(),
                ..Default::default()
            };
            over(&mut s);
            s
        }

        fn view(visible: bool, text: &str, show_button: bool, keep_text: bool) -> BannerView {
            BannerView {
                visible,
                text: text.into(),
                show_button,
                keep_text,
            }
        }

        #[test]
        fn banner_view_hides_the_button_while_an_update_runs() {
            let s = status(|s| {
                s.update_available = true;
                s.progress = Some(state(|p| p.reason = Some("boom".into())));
            });

            assert_eq!(
                update_banner_view(&s, at(5), None),
                view(true, "Replacing files, 5 s", false, true)
            );
        }

        #[test]
        fn banner_view_keeps_the_failure_text_and_still_shows_the_button_when_an_update_is_available()
         {
            let s = status(|s| {
                s.update_available = true;
                s.progress = Some(state(|p| {
                    p.result = "failed".into();
                    p.reason = Some("boom".into());
                }));
            });

            let v = update_banner_view(&s, at(3600), None);

            assert!(v.visible);
            assert_eq!(
                v.text,
                "The update to 3.0.6 failed at replacing files: boom. Still on 3.0.5."
            );
            assert!(v.show_button);
            assert!(v.keep_text);
        }

        #[test]
        fn banner_view_shows_the_failure_without_a_button_when_no_update_is_available() {
            let s = status(|s| s.progress = Some(state(|p| p.result = "rolled-back".into())));

            assert!(!update_banner_view(&s, at(0), None).show_button);
        }

        #[test]
        fn banner_view_shows_ok_for_10_minutes_and_then_falls_back_to_the_offer() {
            let ok = state(|p| {
                p.result = "ok".into();
                p.step = "done".into();
            });
            let plain = status(|s| s.progress = Some(ok.clone()));
            let offered = status(|s| {
                s.update_available = true;
                s.progress = Some(ok);
            });

            assert_eq!(
                update_banner_view(&plain, at(9 * 60), None),
                view(true, "Back on air on 3.0.6.", false, true)
            );
            assert_eq!(
                update_banner_view(&plain, at(11 * 60), None),
                view(false, "", true, false)
            );
            let v = update_banner_view(&offered, at(11 * 60), None);
            assert!(v.visible && v.show_button && !v.keep_text);
        }

        #[test]
        fn banner_view_treats_an_unparsable_step_time_as_past_the_ok_window() {
            let ok = state(|p| {
                p.result = "ok".into();
                p.step = "done".into();
                p.step_started_at = "garbage".into();
            });
            let s = status(|s| s.progress = Some(ok));

            assert_eq!(
                update_banner_view(&s, at(1), None),
                view(false, "", true, false)
            );
        }

        #[test]
        fn banner_view_drops_the_ready_banner_once_the_stream_is_live_and_keeps_back_on_air() {
            let progress = |step: &str| {
                status(|s| {
                    s.progress = Some(state(|p| {
                        p.result = "ok".into();
                        p.step = step.into();
                    }))
                })
            };

            let ready = progress("start");
            let back = progress("done");

            let waiting = update_banner_view(&ready, at(5), Some("stopped"));
            assert!(waiting.visible);
            assert_eq!(waiting.text, "Updated to 3.0.6. Ready to stream.");
            let live = update_banner_view(&ready, at(5), Some("live"));
            assert!(!live.visible && !live.keep_text);
            let air = update_banner_view(&back, at(5), Some("live"));
            assert!(air.visible);
            assert_eq!(air.text, "Back on air on 3.0.6.");
        }

        #[test]
        fn banner_view_hides_a_finished_update_that_neither_came_from_nor_went_to_the_running_version()
         {
            // A failed full update from 3.0.5 to 3.0.6, then an app-only update to 3.0.8.
            let failed = state(|p| {
                p.result = "failed".into();
                p.reason = Some("boom".into());
            });
            let at_version = |current: &str, available: bool| {
                status(|s| {
                    s.current_version = Some(current.into());
                    s.update_available = available;
                    s.progress = Some(failed.clone());
                })
            };

            let later = update_banner_view(&at_version("3.0.8", false), at(5), None);
            let offer = update_banner_view(&at_version("3.0.8", true), at(5), None);
            let still_old = update_banner_view(&at_version("3.0.5", false), at(5), None);

            assert!(!later.visible && !later.keep_text);
            assert!(offer.visible && offer.show_button && !offer.keep_text);
            assert!(still_old.visible && still_old.keep_text);
            assert_eq!(
                still_old.text,
                "The update to 3.0.6 failed at replacing files: boom. Still on 3.0.5."
            );
        }
    }
}
