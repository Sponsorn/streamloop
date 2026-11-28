//! Helpers that live inside the dashboard's admin.js: formatting, form checks and banner texts.

use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::Value;

use crate::api::types::{CacheFile, Playing, PlaylistRef, Supervisor, UpdateStatus};
use crate::logic::status_view::{
    app_download_text, format_time, update_banner_view, update_offer_text,
};

/// A YouTube URL gives its `list` parameter (None when it has none); anything else comes back as typed.
pub fn extract_playlist_id(raw: &str) -> Option<String> {
    if let Ok(url) = url::Url::parse(raw)
        && let Some(host) = url.host_str()
        && (host.contains("youtube.com") || host.contains("youtu.be"))
    {
        return url
            .query_pairs()
            .find(|(k, _)| k == "list")
            .map(|(_, v)| v.into_owned());
    }
    Some(raw.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaylistEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Rows are (id, name). Err holds the index of every row whose id is a video URL.
pub fn collect_playlists(rows: &[(String, String)]) -> Result<Vec<PlaylistEntry>, Vec<usize>> {
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for (i, (raw, name)) in rows.iter().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Some(id) = extract_playlist_id(raw).filter(|id| !id.is_empty()) else {
            bad.push(i);
            continue;
        };
        let name = name.trim();
        out.push(PlaylistEntry {
            id,
            name: (!name.is_empty()).then(|| name.to_string()),
        });
    }
    if bad.is_empty() { Ok(out) } else { Err(bad) }
}

/// Drops blank strings, so the server keeps the saved value instead of storing "".
pub fn without_blanks(obj: Value) -> Value {
    match obj {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| v.as_str() != Some(""))
                .collect(),
        ),
        other => other,
    }
}

pub fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {}s", s % 60)
    } else {
        format!("{s}s")
    }
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

const GB: u64 = 1024 * 1024 * 1024;

pub fn format_gb(n: u64) -> String {
    format!("{:.1} GB", n as f64 / GB as f64)
}

/// GB from 1 GB up, else as `format_bytes`.
pub fn format_size(n: u64) -> String {
    if n >= GB {
        format_gb(n)
    } else {
        format_bytes(n)
    }
}

/// Bytes a second: MB with one decimal from 1 MB/s up, whole KB from 1 KB/s, else whole bytes.
pub fn format_rate(bps: f64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bps >= MB {
        format!("{:.1} MB/s", bps / MB)
    } else if bps >= 1024.0 {
        format!("{:.0} KB/s", bps / 1024.0)
    } else {
        format!("{bps:.0} B/s")
    }
}

/// `1080p30 h264`: the height with the frame rate, then the codec, each a `-` when unknown.
pub fn format_quality(f: &CacheFile) -> String {
    let resolution = f.height.map_or("-".to_string(), |h| {
        let fps = f.fps.map_or(String::new(), |fps| format!("{fps:.0}"));
        format!("{h}p{fps}")
    });
    let codec = f.vcodec.as_deref().filter(|c| !c.is_empty()).unwrap_or("-");
    if resolution == "-" && codec == "-" {
        "-".to_string()
    } else {
        format!("{resolution} {codec}")
    }
}

/// A video's length in the list: no hour part when it is under an hour.
pub fn video_duration(seconds: Option<f64>) -> String {
    match seconds {
        Some(s) if s > 0.0 && s.is_finite() => {
            let t = format_time(Some(s));
            t.strip_prefix("0:").map_or(t.clone(), str::to_string)
        }
        _ => "-".to_string(),
    }
}

pub fn encoder_line(sup: Option<&Supervisor>) -> String {
    let mut text = "Encoder: not running".to_string();
    if let Some(enc) = sup.and_then(|s| s.encoder.as_ref()).filter(|e| e.alive) {
        text = "Encoder: running".to_string();
        if let Some(speed) = enc.speed {
            text.push_str(&format!(", {speed:.2}x speed"));
        }
        if enc.restarts > 0 {
            text.push_str(&format!(
                ", {} restart{}",
                enc.restarts,
                if enc.restarts == 1 { "" } else { "s" }
            ));
        }
    }
    text
}

pub fn now_playing_index(p: &Playing, count: Option<u32>) -> String {
    match p.index {
        Some(i) => match count.filter(|c| *c > 0) {
            Some(c) => format!("{} of {c}", i + 1),
            None => (i + 1).to_string(),
        },
        None => "-".to_string(),
    }
}

pub fn now_playing_time(p: &Playing) -> String {
    match p.position {
        Some(pos) => {
            let total = match p.duration {
                Some(d) if d != 0.0 => format_time(Some(d)),
                _ => "--:--:--".to_string(),
            };
            format!("{} / {total}", format_time(Some(pos)))
        }
        None => "-".to_string(),
    }
}

pub struct NumberField {
    pub key: &'static str,
    pub min: f64,
    pub label: &'static str,
    pub integer: bool,
}

pub const PLAYBACK_FIELDS: [NumberField; 4] = [
    NumberField {
        key: "readAheadSeconds",
        min: 2.0,
        label: "Read-ahead",
        integer: false,
    },
    NumberField {
        key: "cacheBudgetMb",
        min: 50.0,
        label: "Download cache",
        integer: true,
    },
    NumberField {
        key: "freezeSeconds",
        min: 5.0,
        label: "Frozen picture",
        integer: false,
    },
    NumberField {
        key: "silenceSeconds",
        min: 5.0,
        label: "Silence",
        integer: false,
    },
];

pub fn check_number(field: &NumberField, text: &str) -> Result<f64, String> {
    let n = text.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    match n {
        Some(n) if n >= field.min && !(field.integer && n.fract() != 0.0) => Ok(n),
        _ => Err(format!(
            "{} must be {}at least {}.",
            field.label,
            if field.integer {
                "a whole number, "
            } else {
                ""
            },
            field.min as i64
        )),
    }
}

/// The update banner: its text, and the button's label and enabled flag (None while the button is hidden).
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub text: String,
    pub button: Option<(String, bool)>,
}

/// admin.js `renderUpdateBanner`. A version the server left out prints as an empty string where
/// admin.js prints "undefined".
pub fn update_banner(
    status: &UpdateStatus,
    now_ms: i64,
    stream_state: Option<&str>,
) -> Option<Banner> {
    let view = update_banner_view(status, now_ms, stream_state);
    if !view.visible {
        return None;
    }
    let latest = status.latest_version.as_deref().unwrap_or("");
    let own = |text: String| {
        if view.keep_text {
            view.text.clone()
        } else {
            text
        }
    };
    let button = |label: &str, enabled| view.show_button.then(|| (label.to_string(), enabled));
    let (text, button) = if !view.show_button {
        (own(String::new()), None)
    } else {
        match status.status.as_str() {
            "downloading" => (
                own(app_download_text(status)),
                button("Downloading...", false),
            ),
            "extracting" => (
                own(format!("Installing v{latest}...")),
                button("Installing...", false),
            ),
            "ready" => (
                own("Update installed. Restarting...".to_string()),
                button("Restarting...", false),
            ),
            "error" => (
                own(format!(
                    "Update failed: {}",
                    status.error.as_deref().unwrap_or("")
                )),
                button("Retry", true),
            ),
            _ => {
                let label = if status.is_dev_mode {
                    "Dev Mode".to_string()
                } else if status.kind.as_deref() == Some("full") && status.same_version {
                    update_offer_text(status)
                } else {
                    "Update Now".to_string()
                };
                (
                    own(update_offer_text(status)),
                    button(&label, !status.is_dev_mode),
                )
            }
        }
    };
    Some(Banner { text, button })
}

pub fn check_update_result(status: &UpdateStatus) -> String {
    if status.update_available {
        let mut text = format!(
            "v{} is available.",
            status.latest_version.as_deref().unwrap_or("")
        );
        if let Some(n) = status.node_notice.as_deref().filter(|n| !n.is_empty()) {
            text.push(' ');
            text.push_str(n);
        }
        text
    } else {
        format!(
            "Up to date (v{})",
            status.current_version.as_deref().unwrap_or("")
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewLevel {
    Info,
    Warn,
    Error,
}

const EVENT_INFO: [(&str, &str, &str, PreviewLevel); 10] = [
    (
        "error",
        "Video Errors",
        "A video failed and is being retried",
        PreviewLevel::Warn,
    ),
    (
        "skip",
        "Video Skips",
        "A video was skipped after repeated failures",
        PreviewLevel::Warn,
    ),
    (
        "recovery",
        "Recovery",
        "The supervisor is answering again after being down",
        PreviewLevel::Info,
    ),
    (
        "critical",
        "Critical Alerts",
        "The stream cannot recover by itself and needs you",
        PreviewLevel::Error,
    ),
    (
        "resume",
        "Playback Resumed",
        "The stream resumed a video after a restart",
        PreviewLevel::Info,
    ),
    (
        "streamDrop",
        "Encoder Stopped",
        "The encoder stopped and is restarting",
        PreviewLevel::Warn,
    ),
    (
        "streamRestart",
        "Encoder Restarted",
        "The encoder is running again after stopping",
        PreviewLevel::Info,
    ),
    (
        "outputFrozen",
        "Frozen or Silent Output",
        "The picture froze or the audio went silent",
        PreviewLevel::Warn,
    ),
    (
        "twitchMismatch",
        "Twitch Mismatch",
        "The encoder runs but the channel is offline or frozen on Twitch",
        PreviewLevel::Warn,
    ),
    (
        "twitchRestart",
        "Twitch Restarted",
        "The stream restarted after a Twitch mismatch",
        PreviewLevel::Info,
    ),
];

const SAMPLE_VARS: [(&str, &str); 11] = [
    ("videoIndex", "3"),
    ("videoId", "dQw4w9WgXcQ"),
    ("errorCode", "early-eof"),
    ("attempt", "2"),
    ("reason", "unavailable: Video unavailable"),
    ("step", "Supervisor answering again"),
    ("message", "The encoder has stopped 3 times in a row."),
    ("maxAttempts", "3"),
    ("attempts", "1"),
    ("kind", "freeze"),
    ("channel", "mychannel"),
];

/// (label, description, level); an unknown key shows as itself.
pub fn event_info(key: &str) -> (String, String, PreviewLevel) {
    EVENT_INFO.iter().find(|(k, ..)| *k == key).map_or(
        (key.to_string(), String::new(), PreviewLevel::Info),
        |(_, l, d, lv)| (l.to_string(), d.to_string(), *lv),
    )
}

/// Replaces `{name}` (ASCII letters, digits, underscore) with its sample value; unknown names stay.
pub fn render_template(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if name_len > 0 && after.as_bytes().get(name_len) == Some(&b'}') {
            let name = &after[..name_len];
            match SAMPLE_VARS.iter().find(|(k, _)| *k == name) {
                Some((_, v)) => out.push_str(v),
                None => out.push_str(&rest[open..open + name_len + 2]),
            }
            rest = &after[name_len + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

pub fn preview_colour(level: PreviewLevel) -> u32 {
    match level {
        PreviewLevel::Info => 0x3498db,
        PreviewLevel::Warn => 0xf1c40f,
        PreviewLevel::Error => 0xe74c3c,
    }
}

pub fn preview_title(level: PreviewLevel) -> String {
    let emoji = match level {
        PreviewLevel::Info => "ℹ️",
        PreviewLevel::Warn => "⚠️",
        PreviewLevel::Error => "🚨",
    };
    format!("{emoji} StreamLoop")
}

/// (state text, button, placeholder)
pub fn stream_key_state(is_set: bool) -> (&'static str, &'static str, &'static str) {
    if is_set {
        (
            "A stream key is set.",
            "Replace",
            "Paste a new key to replace it",
        )
    } else {
        (
            "No stream key is set. The stream cannot start without one.",
            "Set",
            "Paste your Twitch stream key",
        )
    }
}

pub fn playlist_option_label(p: &PlaylistRef, index: usize, playing: usize) -> String {
    let base = p.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&p.id);
    if index == playing {
        format!("{base} (playing)")
    } else {
        base.to_string()
    }
}

pub fn local_hms(ts: &str) -> String {
    local_format(ts, "%H:%M:%S")
}

pub fn local_hm(ts: &str) -> String {
    local_format(ts, "%H:%M")
}

pub fn local_date_time(ts: &str) -> String {
    local_format(ts, "%Y-%m-%d %H:%M")
}

fn local_format(ts: &str, fmt: &str) -> String {
    match DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt.with_timezone(&Local).format(fmt).to_string(),
        Err(_) => ts.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Encoder;
    use crate::api::types::UpdateProgress;
    use serde_json::json;

    fn rows(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn extracts_playlist_ids() {
        assert_eq!(
            extract_playlist_id("https://www.youtube.com/playlist?list=PLabc"),
            Some("PLabc".into())
        );
        assert_eq!(
            extract_playlist_id("https://youtu.be/xyz?list=PLq"),
            Some("PLq".into())
        );
        assert_eq!(
            extract_playlist_id("https://www.youtube.com/watch?v=abc"),
            None
        );
        assert_eq!(extract_playlist_id("PLabc"), Some("PLabc".into()));
        assert_eq!(
            extract_playlist_id("https://example.com/?list=PLx"),
            Some("https://example.com/?list=PLx".into())
        );
    }

    #[test]
    fn collects_playlists_skipping_blanks_and_trimming_names() {
        let got =
            collect_playlists(&rows(&[("  PLa ", " Mine "), ("", "x"), ("PLb", "  ")])).unwrap();
        assert_eq!(
            got,
            vec![
                PlaylistEntry {
                    id: "PLa".into(),
                    name: Some("Mine".into())
                },
                PlaylistEntry {
                    id: "PLb".into(),
                    name: None
                },
            ]
        );
        assert_eq!(
            serde_json::to_value(&got).unwrap(),
            json!([{"id": "PLa", "name": "Mine"}, {"id": "PLb"}])
        );
    }

    #[test]
    fn reports_every_row_holding_a_video_url() {
        let v = "https://www.youtube.com/watch?v=abc";
        let got = collect_playlists(&rows(&[(v, ""), ("PLa", ""), (v, "")]));
        assert_eq!(got, Err(vec![0, 2]));
    }

    #[test]
    fn keeps_non_ascii_names_whole() {
        let got = collect_playlists(&rows(&[("PLa", "  Låtar 🎵 日本 ")])).unwrap();
        assert_eq!(got[0].name.as_deref(), Some("Låtar 🎵 日本"));
    }

    #[test]
    fn drops_blank_strings_only() {
        let got = without_blanks(json!({"a": "", "b": "x", "c": false}));
        assert_eq!(got, json!({"b": "x", "c": false}));
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59_000), "59s");
        assert_eq!(format_duration(61_000), "1m 1s");
        assert_eq!(format_duration(3_661_000), "1h 1m");
        assert_eq!(format_duration(180_000_000), "50h 0m");
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(3_145_728), "3.0 MB");
    }

    #[test]
    fn gb_has_one_decimal() {
        assert_eq!(format_gb(15_246_000_000), "14.2 GB");
        assert_eq!(format_gb(32_212_254_720), "30.0 GB");
    }

    #[test]
    fn a_size_reads_in_gb_from_one_gb_and_in_mb_below() {
        assert_eq!(format_size(15_246_000_000), "14.2 GB");
        assert_eq!(format_size(314_572_800), "300.0 MB");
    }

    #[test]
    fn rate_reads_in_mb_or_kb_per_second() {
        assert_eq!(format_rate(2_411_725.0), "2.3 MB/s");
        assert_eq!(format_rate(524_288.0), "512 KB/s");
        assert_eq!(format_rate(491.18), "491 B/s");
    }

    #[test]
    fn quality_reads_height_fps_and_codec_and_dashes_what_is_unknown() {
        let file = |height: Option<u32>, fps: Option<f64>, vcodec: Option<&str>| CacheFile {
            height,
            fps,
            vcodec: vcodec.map(str::to_string),
            ..CacheFile::default()
        };

        assert_eq!(
            format_quality(&file(Some(1080), Some(29.97), Some("h264"))),
            "1080p30 h264"
        );
        assert_eq!(
            format_quality(&file(Some(720), None, Some("vp9"))),
            "720p vp9"
        );
        assert_eq!(
            format_quality(&file(None, Some(30.0), Some("av1"))),
            "- av1"
        );
        assert_eq!(
            format_quality(&file(Some(480), Some(25.0), None)),
            "480p25 -"
        );
        assert_eq!(format_quality(&file(None, None, None)), "-");
    }

    #[test]
    fn formats_video_durations() {
        assert_eq!(video_duration(Some(754.0)), "12:34");
        assert_eq!(video_duration(Some(3754.0)), "1:02:34");
        assert_eq!(video_duration(None), "-");
    }

    #[test]
    fn describes_the_encoder() {
        let sup = |alive, speed, restarts| Supervisor {
            encoder: Some(Encoder {
                alive,
                speed,
                restarts,
            }),
            ..Default::default()
        };
        assert_eq!(encoder_line(None), "Encoder: not running");
        assert_eq!(
            encoder_line(Some(&sup(false, Some(1.0), 0))),
            "Encoder: not running"
        );
        assert_eq!(
            encoder_line(Some(&sup(true, Some(0.987), 1))),
            "Encoder: running, 0.99x speed, 1 restart"
        );
        assert_eq!(
            encoder_line(Some(&sup(true, Some(0.987), 2))),
            "Encoder: running, 0.99x speed, 2 restarts"
        );
    }

    #[test]
    fn describes_now_playing() {
        let p = Playing {
            index: Some(3),
            position: Some(65.0),
            duration: Some(3600.0),
            ..Default::default()
        };
        assert_eq!(now_playing_index(&p, Some(300)), "4 of 300");
        assert_eq!(now_playing_index(&p, None), "4");
        assert_eq!(now_playing_index(&Playing::default(), Some(300)), "-");
        assert_eq!(now_playing_time(&p), "0:01:05 / 1:00:00");
        let open = Playing {
            duration: None,
            ..p
        };
        assert_eq!(now_playing_time(&open), "0:01:05 / --:--:--");
        assert_eq!(now_playing_time(&Playing::default()), "-");
    }

    #[test]
    fn checks_playback_numbers() {
        let [read_ahead, cache, ..] = &PLAYBACK_FIELDS;
        assert_eq!(
            check_number(read_ahead, "1"),
            Err("Read-ahead must be at least 2.".into())
        );
        assert_eq!(
            check_number(cache, "50.5"),
            Err("Download cache must be a whole number, at least 50.".into())
        );
        assert!(check_number(read_ahead, "").is_err());
        assert!(check_number(read_ahead, "2,5").is_err());
        assert_eq!(check_number(read_ahead, " 20 "), Ok(20.0));
    }

    fn status(update_available: bool, state: &str) -> UpdateStatus {
        UpdateStatus {
            current_version: Some("3.0.5".into()),
            latest_version: Some("3.0.6".into()),
            update_available,
            status: state.into(),
            error: Some("boom".into()),
            ..Default::default()
        }
    }

    const NOW: i64 = 1_794_132_000_000;

    fn b(text: &str, button: &str, enabled: bool) -> Option<Banner> {
        Some(Banner {
            text: text.into(),
            button: Some((button.into(), enabled)),
        })
    }

    fn banner(status: &UpdateStatus) -> Option<Banner> {
        update_banner(status, NOW, None)
    }

    #[test]
    fn banner_per_state() {
        assert_eq!(
            banner(&status(true, "idle")),
            b(
                "Version 3.0.6 is available (current: 3.0.5)",
                "Update Now",
                true
            )
        );
        let dev = UpdateStatus {
            is_dev_mode: true,
            ..status(true, "idle")
        };
        assert_eq!(
            banner(&dev),
            b(
                "Version 3.0.6 is available (current: 3.0.5)",
                "Dev Mode",
                false
            )
        );
        assert_eq!(
            banner(&status(true, "downloading")),
            b("Downloading v3.0.6...", "Downloading...", false)
        );
        assert_eq!(
            banner(&status(true, "extracting")),
            b("Installing v3.0.6...", "Installing...", false)
        );
        assert_eq!(
            banner(&status(true, "ready")),
            b("Update installed. Restarting...", "Restarting...", false)
        );
        assert_eq!(
            banner(&status(true, "error")),
            b("Update failed: boom", "Retry", true)
        );
        assert_eq!(banner(&status(false, "idle")), None);
    }

    fn progress(result: &str) -> UpdateProgress {
        UpdateProgress {
            from: "3.0.5".into(),
            to: "3.0.6".into(),
            step: "swap".into(),
            result: result.into(),
            reason: Some("boom".into()),
            started_at: "2026-11-08T10:00:00.000Z".into(),
            step_started_at: "2026-11-08T10:00:00.000Z".into(),
            ..Default::default()
        }
    }

    #[test]
    fn banner_of_a_running_full_update_shows_its_progress_and_no_button() {
        let s = UpdateStatus {
            current_version: Some("3.0.5".into()),
            progress: Some(progress("running")),
            ..status(true, "idle")
        };

        let shown = update_banner(&s, NOW + 5_000, None);

        assert_eq!(
            shown,
            Some(Banner {
                text: "Replacing files, 5 s".into(),
                button: None
            })
        );
    }

    #[test]
    fn banner_of_a_failed_update_keeps_the_failure_text_and_offers_update_now() {
        let s = UpdateStatus {
            current_version: Some("3.0.5".into()),
            progress: Some(progress("failed")),
            ..status(true, "idle")
        };

        let shown = update_banner(&s, NOW + 3_600_000, None);

        assert_eq!(
            shown,
            b(
                "The update to 3.0.6 failed at replacing files: boom. Still on 3.0.5.",
                "Update Now",
                true
            )
        );
    }

    #[test]
    fn banner_of_a_same_version_full_offer_puts_the_offer_on_the_button() {
        let s = UpdateStatus {
            current_version: Some("3.0.5".into()),
            latest_version: Some("3.0.5".into()),
            kind: Some("full".into()),
            parts: vec!["pot".into()],
            same_version: true,
            ..status(true, "idle")
        };

        assert_eq!(
            banner(&s),
            b("Update parts to 3.0.5", "Update parts to 3.0.5", true)
        );
    }

    #[test]
    fn banner_of_a_full_offer_names_the_parts_and_keeps_update_now() {
        let s = UpdateStatus {
            kind: Some("full".into()),
            parts: vec!["node".into()],
            ..status(true, "idle")
        };

        assert_eq!(
            banner(&s),
            b(
                "Full update: the stream goes off air while these parts are replaced: Node.",
                "Update Now",
                true
            )
        );
    }

    #[test]
    fn banner_of_an_app_only_download_shows_the_percentage() {
        let s = UpdateStatus {
            download_percent: Some(42.0),
            ..status(true, "downloading")
        };

        assert_eq!(
            banner(&s),
            b("Downloading v3.0.6, 42%...", "Downloading...", false)
        );
    }

    #[test]
    fn check_result_texts() {
        assert_eq!(
            check_update_result(&status(false, "idle")),
            "Up to date (v3.0.5)"
        );
        assert_eq!(
            check_update_result(&status(true, "idle")),
            "v3.0.6 is available."
        );
        let noted = UpdateStatus {
            node_notice: Some("Install the full ZIP.".into()),
            ..status(true, "idle")
        };
        assert_eq!(
            check_update_result(&noted),
            "v3.0.6 is available. Install the full ZIP."
        );
    }

    #[test]
    fn renders_templates_with_sample_values() {
        assert_eq!(
            render_template("Video {videoIndex} failed: {errorCode} {nope}"),
            "Video 3 failed: early-eof {nope}"
        );
        assert_eq!(
            render_template("Hej {channel} 🎥 åäö"),
            "Hej mychannel 🎥 åäö"
        );
        assert_eq!(render_template("a { b {kind} {} {"), "a { b freeze {} {");
    }

    #[test]
    fn looks_up_event_info() {
        assert_eq!(
            event_info("streamDrop"),
            (
                "Encoder Stopped".into(),
                "The encoder stopped and is restarting".into(),
                PreviewLevel::Warn
            )
        );
        assert_eq!(
            event_info("brandNew"),
            ("brandNew".into(), String::new(), PreviewLevel::Info)
        );
    }

    #[test]
    fn preview_look() {
        assert_eq!(preview_colour(PreviewLevel::Info), 0x3498db);
        assert_eq!(preview_colour(PreviewLevel::Warn), 0xf1c40f);
        assert_eq!(preview_colour(PreviewLevel::Error), 0xe74c3c);
        assert_eq!(preview_title(PreviewLevel::Info), "ℹ️ StreamLoop");
        assert_eq!(preview_title(PreviewLevel::Warn), "⚠️ StreamLoop");
        assert_eq!(preview_title(PreviewLevel::Error), "🚨 StreamLoop");
    }

    #[test]
    fn stream_key_texts() {
        assert_eq!(
            stream_key_state(true),
            (
                "A stream key is set.",
                "Replace",
                "Paste a new key to replace it"
            )
        );
        assert_eq!(
            stream_key_state(false),
            (
                "No stream key is set. The stream cannot start without one.",
                "Set",
                "Paste your Twitch stream key"
            )
        );
    }

    #[test]
    fn labels_playlist_options() {
        let named = PlaylistRef {
            id: "PLa".into(),
            name: Some("Mine".into()),
        };
        let bare = PlaylistRef {
            id: "PLb".into(),
            name: None,
        };
        assert_eq!(playlist_option_label(&named, 0, 0), "Mine (playing)");
        assert_eq!(playlist_option_label(&bare, 1, 0), "PLb");
    }

    #[test]
    fn formats_local_times() {
        assert_eq!(local_hms("not a time"), "not a time");
        assert_eq!(local_date_time("Låtar 🎵"), "Låtar 🎵");
        let ts = "2026-11-08T12:34:56Z";
        let local = DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(local_hms(ts), local.format("%H:%M:%S").to_string());
        assert_eq!(
            local_date_time(ts),
            local.format("%Y-%m-%d %H:%M").to_string()
        );
    }
}
