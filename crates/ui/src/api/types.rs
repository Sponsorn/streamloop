//! Typed answers of the dashboard's REST calls. A missing field reads as its default and an unknown one is ignored.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::Value;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    pub stream: Stream,
    pub supervisor: Option<Supervisor>,
    #[serde(deserialize_with = "lenient")]
    pub launcher: String,
    pub twitch: Option<Twitch>,
    #[serde(deserialize_with = "lenient")]
    pub first_run: bool,
    /// Negative after the clock steps back, which reads as 0.
    #[serde(deserialize_with = "lenient")]
    pub uptime_ms: u64,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Stream {
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Supervisor {
    pub playing: Option<Playing>,
    pub encoder: Option<Encoder>,
    pub youtube: Option<Youtube>,
    #[serde(deserialize_with = "lenient")]
    pub playlist_count: Option<u32>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Playing {
    pub index: Option<u32>,
    pub playlist_id: Option<String>,
    pub id: Option<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub position: Option<f64>,
    pub duration: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Encoder {
    #[serde(deserialize_with = "lenient")]
    pub alive: bool,
    #[serde(deserialize_with = "lenient")]
    pub speed: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub restarts: u32,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Youtube {
    pub breaker: Option<String>,
    pub retry_at: Option<String>,
    pub cookie_problem: Option<String>,
    pub bot: Option<bool>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Twitch {
    #[serde(deserialize_with = "lenient")]
    pub enabled: bool,
    #[serde(deserialize_with = "lenient")]
    pub channel_live: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    pub consecutive_mismatches: u32,
    #[serde(deserialize_with = "lenient")]
    pub mismatch_threshold: u32,
    pub readback: Option<Readback>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Readback {
    pub state: String,
    pub last_check_at: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Event {
    pub timestamp: String,
    pub message: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub status: String,
    pub error: Option<String>,
    pub is_dev_mode: bool,
    pub node_notice: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub parts: Vec<String>,
    #[serde(deserialize_with = "lenient")]
    pub same_version: bool,
    #[serde(deserialize_with = "lenient")]
    pub download_percent: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub progress: Option<UpdateProgress>,
    #[serde(deserialize_with = "lenient")]
    pub swap_log: Vec<SwapLine>,
}

/// `GET /api/cache`: the supervisor's report, or `None` when no supervisor answered it.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheReply {
    #[serde(deserialize_with = "lenient")]
    pub cache: Option<CacheReport>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheReport {
    #[serde(deserialize_with = "lenient")]
    pub cache_bytes: u64,
    #[serde(deserialize_with = "lenient")]
    pub budget_bytes: u64,
    #[serde(deserialize_with = "lenient")]
    pub next: Option<CacheNext>,
    #[serde(deserialize_with = "lenient")]
    pub videos: Vec<CacheVideo>,
    #[serde(deserialize_with = "lenient")]
    pub deletions: Vec<CacheDeletion>,
}

/// What the downloader does next: `when` says why or when, and the video it names, if any.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheNext {
    #[serde(deserialize_with = "lenient")]
    pub when: String,
    #[serde(deserialize_with = "lenient")]
    pub at: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub title: Option<String>,
}

/// A video from the one on air through the look-ahead. `index` counts within its playlist.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheVideo {
    #[serde(deserialize_with = "lenient")]
    pub index: u32,
    #[serde(deserialize_with = "lenient")]
    pub playlist_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub state: String,
    #[serde(deserialize_with = "lenient")]
    pub source: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub bytes: Option<u64>,
    #[serde(deserialize_with = "lenient")]
    pub looked_up: bool,
    #[serde(deserialize_with = "lenient")]
    pub download: Option<CacheDownload>,
    #[serde(deserialize_with = "lenient")]
    pub retry_at: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub room_at: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub file: Option<CacheFile>,
}

/// `part` counts the streams of the download from 1: video, then audio.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheDownload {
    #[serde(deserialize_with = "lenient")]
    pub percent: f64,
    #[serde(deserialize_with = "lenient")]
    pub speed_bps: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub eta_s: Option<u64>,
    #[serde(deserialize_with = "lenient")]
    pub part: u32,
    #[serde(deserialize_with = "lenient")]
    pub parts: u32,
}

/// What `cache.json` holds about a file on disk.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheFile {
    #[serde(deserialize_with = "lenient")]
    pub height: Option<u32>,
    #[serde(deserialize_with = "lenient")]
    pub fps: Option<f64>,
    #[serde(deserialize_with = "lenient")]
    pub vcodec: Option<String>,
    /// The file's size over the video's length.
    #[serde(deserialize_with = "lenient")]
    pub kbps: Option<u64>,
    #[serde(deserialize_with = "lenient")]
    pub format: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub client: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub finished_at: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheDeletion {
    #[serde(deserialize_with = "lenient")]
    pub at: String,
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    #[serde(deserialize_with = "lenient")]
    pub title: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub bytes: u64,
    #[serde(deserialize_with = "lenient")]
    pub reason: String,
}

/// A field of the wrong type reads as its default, so the rest of the answer still reads, as admin.js
/// tolerates it with `typeof` checks and `|| []`.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

/// `update-state.json` as `/api/update/status` serves it; `baseline` is left out.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateProgress {
    pub from: String,
    pub to: String,
    pub parts: Vec<String>,
    pub folders: Vec<String>,
    pub step: String,
    pub result: String,
    pub reason: Option<String>,
    pub started_at: String,
    pub step_started_at: String,
    pub download_percent: Option<f64>,
}

/// One line of the swap log, from `readSwapLog`.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct SwapLine {
    pub folder: String,
    pub status: String,
    pub detail: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistRef {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Video {
    pub index: u32,
    pub id: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoPage {
    pub videos: Vec<Video>,
    pub total: usize,
    pub page: u32,
    pub per_page: u32,
    pub current_index: Option<u32>,
    pub playing: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupReport {
    pub at: String,
    pub encoder: SetupEncoder,
    pub cache: SetupCache,
    pub dependencies: Vec<Dependency>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupEncoder {
    pub chosen: String,
    pub software: bool,
    pub works: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupCache {
    pub free_mb: u64,
    pub recommended_mb: u64,
    pub warning: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Dependency {
    pub name: String,
    pub ok: bool,
    pub path: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AutostartStatus {
    pub enabled: bool,
    pub target: Option<String>,
    pub this_install: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct LogFile {
    pub name: String,
    pub size: u64,
    pub mtime: String,
    pub source: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SaveResult {
    pub config: Value,
    pub restart_required: bool,
    pub reload_error: Option<String>,
    pub reload_error_code: Option<String>,
}

/// Both maps keep the server's key order, which is the order the dashboard shows.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct DiscordDefaults {
    pub templates: IndexMap<String, String>,
    pub variables: IndexMap<String, Vec<String>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct YtdlpUpdate {
    pub version: String,
}

/// The fields the settings forms show, read from the raw `GET /api/config` value.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub playlists: Vec<PlaylistRef>,
    pub supervisor: SupervisorConfig,
    pub discord: DiscordConfig,
    pub twitch_liveness_enabled: bool,
    pub twitch_client_id: String,
    pub twitch_client_secret: String,
    pub twitch_channel: String,
    pub auto_start_stream: bool,
    pub ytdl_cookies_from_browser: String,
}

#[derive(Clone, Debug, Default)]
pub struct SupervisorConfig {
    pub ingest_url: String,
    pub stream_key_set: bool,
    pub encoder: String,
    pub overlay: bool,
    pub player_client: String,
    pub read_ahead_seconds: f64,
    pub cache_budget_mb: f64,
    pub freeze_seconds: f64,
    pub silence_seconds: f64,
}

#[derive(Clone, Debug, Default)]
pub struct DiscordConfig {
    pub webhook_url: String,
    pub bot_name: String,
    pub avatar_url: String,
    pub role_ping: String,
    pub templates: IndexMap<String, String>,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or_default()
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or_default()
}

/// The dashboard treats anything but an explicit `false` as on.
fn flag_default_on(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) != Some(false)
}

impl Config {
    pub fn from_value(v: &Value) -> Config {
        let playlists = v
            .get("playlists")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|p| PlaylistRef {
                        id: text(p, "id"),
                        name: p.get("name").and_then(Value::as_str).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let null = Value::Null;
        let s = v.get("supervisor").unwrap_or(&null);
        let d = v.get("discord").unwrap_or(&null);
        let map = |key: &str| d.get(key).and_then(Value::as_object);
        Config {
            playlists,
            supervisor: SupervisorConfig {
                ingest_url: text(s, "ingestUrl"),
                stream_key_set: flag(s, "streamKeySet"),
                encoder: text(s, "encoder"),
                overlay: flag_default_on(s, "overlay"),
                player_client: text(s, "playerClient"),
                read_ahead_seconds: number(s, "readAheadSeconds"),
                cache_budget_mb: number(s, "cacheBudgetMb"),
                freeze_seconds: number(s, "freezeSeconds"),
                silence_seconds: number(s, "silenceSeconds"),
            },
            discord: DiscordConfig {
                webhook_url: text(d, "webhookUrl"),
                bot_name: text(d, "botName"),
                avatar_url: text(d, "avatarUrl"),
                role_ping: text(d, "rolePing"),
                templates: map("templates")
                    .map(|m| {
                        m.iter()
                            .map(|(k, x)| (k.clone(), x.as_str().unwrap_or_default().to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            twitch_liveness_enabled: flag(v, "twitchLivenessEnabled"),
            twitch_client_id: text(v, "twitchClientId"),
            twitch_client_secret: text(v, "twitchClientSecret"),
            twitch_channel: text(v, "twitchChannel"),
            auto_start_stream: flag_default_on(v, "autoStartStream"),
            ytdl_cookies_from_browser: text(v, "ytdlCookiesFromBrowser"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const STUB_STATUS: &str = r#"{
        "stream": {"state": "live", "reason": null},
        "supervisor": {
            "state": "playing", "reason": null, "streamKeyProblem": null,
            "playing": {"index": 0, "playlistId": "PLone", "id": "abc123", "title": "First video", "source": "file", "position": 12.5, "duration": 300},
            "encoder": {"alive": true, "speed": 1.0, "cpuPercent": 1.6, "restarts": 0, "generation": 1, "mediaTime": 42.1},
            "recovery": {"step": "none", "retries": 0},
            "watchdog": {"last": null},
            "hold": null,
            "youtube": {"breaker": "closed", "retryAt": null, "cookieProblem": null},
            "playlistCount": 4,
            "tokenServer": {"state": "running", "restarts": 0}
        },
        "launcher": "running",
        "twitch": {"enabled": true, "channelLive": true, "consecutiveMismatches": 0, "mismatchThreshold": 3,
            "readback": {"state": "moving", "lastCheckAt": "2026-11-08T10:00:00.000Z"}},
        "firstRun": false,
        "systemMemory": {"totalGB": 16.0, "freeGB": 8.0, "usedGB": 8.0, "usedPercent": 50},
        "uptimeMs": 123456
    }"#;

    #[test]
    fn status_reads_the_stub_backends_answer() {
        let status: Status = serde_json::from_str(STUB_STATUS).unwrap();

        assert_eq!(status.stream.state, "live");
        assert_eq!(
            status
                .supervisor
                .as_ref()
                .unwrap()
                .playing
                .as_ref()
                .unwrap()
                .index,
            Some(0)
        );
        assert_eq!(status.twitch.unwrap().readback.unwrap().state, "moving");
        assert_eq!(status.launcher, "running");
        assert_eq!(status.uptime_ms, 123456);
    }

    #[test]
    fn status_reads_an_empty_object() {
        let status: Status = serde_json::from_str("{}").unwrap();

        assert_eq!(status.stream.state, "");
        assert!(status.supervisor.is_none() && status.twitch.is_none());
        assert_eq!(status.uptime_ms, 0);
    }

    /// The stub's status with the field at `pointer` set to `value`, and with that field left out.
    fn with_and_without(
        pointer: &str,
        value: Value,
    ) -> (Result<Status, serde_json::Error>, Status) {
        let mut raw: Value = serde_json::from_str(STUB_STATUS).unwrap();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        *raw.pointer_mut(pointer).unwrap() = value;
        let with = serde_json::from_value(raw.clone());
        raw.pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        (with, serde_json::from_value(raw).unwrap())
    }

    #[test]
    fn status_reads_a_negative_uptime_as_zero() {
        let (status, without) = with_and_without("/uptimeMs", json!(-5000));

        let status = status.unwrap();
        assert_eq!(status.uptime_ms, 0);
        assert_eq!(format!("{status:?}"), format!("{without:?}"));
        assert_eq!(
            status.supervisor.unwrap().playing.unwrap().title.as_deref(),
            Some("First video")
        );
    }

    #[test]
    fn one_wrongly_typed_scalar_leaves_the_rest_of_the_status_readable() {
        for (pointer, wrong) in [
            ("/launcher", json!(5)),
            ("/supervisor/playlistCount", json!("4")),
            ("/supervisor/encoder/alive", json!("yes")),
            ("/twitch/mismatchThreshold", json!(2.5)),
        ] {
            let (status, without) = with_and_without(pointer, wrong);

            let status = status.unwrap_or_else(|err| panic!("{pointer}: {err}"));
            assert_eq!(format!("{status:?}"), format!("{without:?}"), "{pointer}");
            assert_eq!(status.stream.state, "live", "{pointer}");
        }
    }

    #[test]
    fn status_ignores_fields_it_does_not_know() {
        let raw = r#"{"stream": {"state": "live"}, "version": "3.1.0", "newThing": 5,
            "supervisor": {"hold": {"until": 1}, "tokenServer": {}, "encoder": {"alive": true, "uptimeS": 5}}}"#;

        let status: Status = serde_json::from_str(raw).unwrap();

        assert!(status.supervisor.unwrap().encoder.unwrap().alive);
    }

    #[test]
    fn update_status_reads_every_banner_state() {
        for state in [
            "idle",
            "checking",
            "downloading",
            "extracting",
            "ready",
            "error",
        ] {
            let raw = json!({"status": state, "isDevMode": true, "nodeNotice": "Install the full ZIP", "updateAvailable": true}).to_string();

            let update: UpdateStatus = serde_json::from_str(&raw).unwrap();

            assert_eq!(update.status, state);
            assert!(update.is_dev_mode && update.update_available);
            assert_eq!(update.node_notice.as_deref(), Some("Install the full ZIP"));
        }
    }

    #[test]
    fn update_status_reads_the_full_update_fields() {
        let raw = json!({
            "status": "idle", "updateAvailable": true, "kind": "full", "parts": ["node", "pot"],
            "sameVersion": true, "downloadPercent": 42.5,
            "progress": {
                "from": "3.0.5", "to": "3.0.6", "parts": ["node"], "folders": ["node"], "step": "swap",
                "result": "running", "reason": null, "startedAt": "2026-11-08T10:00:00.000Z",
                "stepStartedAt": "2026-11-08T10:00:05.000Z", "baseline": {"encoder": "libx264", "works": true},
                "downloadPercent": 7
            },
            "swapLog": [{"folder": "node", "status": "retry", "detail": "in use"}]
        })
        .to_string();

        let update: UpdateStatus = serde_json::from_str(&raw).unwrap();

        assert_eq!(update.kind.as_deref(), Some("full"));
        assert_eq!(update.parts, ["node", "pot"]);
        assert!(update.same_version);
        assert_eq!(update.download_percent, Some(42.5));
        let progress = update.progress.unwrap();
        assert_eq!(
            (
                progress.from.as_str(),
                progress.to.as_str(),
                progress.step.as_str()
            ),
            ("3.0.5", "3.0.6", "swap")
        );
        assert_eq!(progress.result, "running");
        assert_eq!(progress.reason, None);
        assert_eq!(progress.step_started_at, "2026-11-08T10:00:05.000Z");
        assert_eq!(progress.download_percent, Some(7.0));
        assert_eq!(
            update.swap_log,
            [SwapLine {
                folder: "node".into(),
                status: "retry".into(),
                detail: "in use".into()
            }]
        );
    }

    #[test]
    fn update_status_without_the_full_update_fields_takes_the_defaults() {
        let update: UpdateStatus =
            serde_json::from_str(r#"{"status": "idle", "progress": null}"#).unwrap();

        assert_eq!(update.kind, None);
        assert!(update.parts.is_empty() && !update.same_version && update.swap_log.is_empty());
        assert!(update.download_percent.is_none() && update.progress.is_none());
    }

    #[test]
    fn update_status_reads_a_wrongly_typed_full_update_field_as_its_default() {
        let raw = json!({
            "currentVersion": "3.0.5", "latestVersion": "3.0.6", "updateAvailable": true, "status": "idle",
            "kind": 5, "parts": null, "sameVersion": "yes", "downloadPercent": "42", "progress": 5,
            "swapLog": null
        })
        .to_string();

        let update: UpdateStatus = serde_json::from_str(&raw).unwrap();

        assert_eq!(update.latest_version.as_deref(), Some("3.0.6"));
        assert!(update.update_available);
        assert_eq!(update.kind, None);
        assert!(update.parts.is_empty() && update.swap_log.is_empty());
        assert!(!update.same_version);
        assert_eq!(update.download_percent, None);
        assert_eq!(update.progress, None);
    }

    #[test]
    fn auto_start_stream_is_on_unless_the_config_says_false() {
        assert!(Config::from_value(&json!({})).auto_start_stream);
        assert!(Config::from_value(&json!({"autoStartStream": true})).auto_start_stream);
        assert!(!Config::from_value(&json!({"autoStartStream": false})).auto_start_stream);
    }

    #[test]
    fn setup_report_reads_null_as_no_report() {
        let report = serde_json::from_str::<Option<SetupReport>>("null");

        assert!(matches!(report, Ok(None)));
    }

    #[test]
    fn discord_defaults_keep_the_servers_key_order() {
        let keys = [
            "error",
            "skip",
            "recovery",
            "critical",
            "resume",
            "streamDrop",
            "streamRestart",
            "outputFrozen",
            "twitchMismatch",
            "twitchRestart",
        ];
        let templates: String = keys
            .iter()
            .map(|k| format!("\"{k}\": \"t\""))
            .collect::<Vec<_>>()
            .join(",");
        let raw = format!(
            "{{\"templates\": {{{templates}}}, \"variables\": {{\"skip\": [\"b\", \"a\"], \"error\": []}}}}"
        );

        let defaults: DiscordDefaults = serde_json::from_str(&raw).unwrap();

        assert_eq!(
            defaults
                .templates
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            keys
        );
        assert_eq!(
            defaults
                .variables
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["skip", "error"]
        );
    }

    #[test]
    fn config_defaults_overlay_to_true() {
        let config = Config::from_value(&json!({}));

        assert!(config.supervisor.overlay);
        assert!(!config.twitch_liveness_enabled);
    }
}
