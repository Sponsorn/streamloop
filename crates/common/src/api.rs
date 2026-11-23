//! The supervisor's HTTP API as the control process reads and writes it (127.0.0.1:4417).
//!
//! Wire names are the supervisor's own camelCase. Every field is optional and unknown fields are
//! ignored, so a newer supervisor never breaks the reader. Each field the supervisor writes is
//! named here, so a field added on one side only fails the round-trip test in the supervisor.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A key that may be absent (outer `None`), `null` (`Some(None)`) or set (`Some(Some(_))`).
pub type Field<T> = Option<Option<T>>;

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Field<T>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SupervisorStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub reason: Field<String>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub stream_key_problem: Field<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playing: Option<Playing>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub timeline: Field<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub read_ahead_seconds: Field<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub cache_bytes: Field<Value>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub downloading: Field<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<EncoderStatus>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub cpu: Field<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_server: Option<TokenServer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<Watchdog>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_count: Option<usize>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub hold: Field<Hold>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub youtube: Option<Youtube>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Playing {
    /// Counts within the list on air.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// Absent from a supervisor before lists were named, `null` until a list has loaded.
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub playlist_id: Field<String>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub id: Field<String>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub title: Field<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<f64>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub duration: Field<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spare_ready: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EncoderStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restarts: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_time: Option<f64>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub speed: Field<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// Whole seconds since the running encoder started; `null` with no encoder.
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub uptime_s: Field<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TokenServer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restarts: Option<u64>,
    /// Which server answers: its `implementation`, else `bgutil VERSION`; `null` until it has answered.
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub server: Field<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recovery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Watchdog {
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub last: Field<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Hold {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Youtube {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breaker: Option<String>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub retry_at: Field<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failures: Option<u64>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub cookie_problem: Field<String>,
    /// The breaker's last failure was YouTube's bot check, not lookups failing; older supervisors leave it out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlCmd {
    Next,
    Prev,
    Restart,
    Jump,
    RestartEncoder,
}

/// Field order is the wire order: cmd, index, reason, id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub cmd: ControlCmd,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// The reply to `next`, `prev`, `restart` and `jump` carries the target; `restart-encoder` carries
/// `accepted` and, while a restart is already pending, `alreadyRestarting`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ControlReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub already_restarting: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut_offset: Option<f64>,
}

/// Field order is the wire order: playlist, index, id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReloadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReloadReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_required: Option<bool>,
    #[serde(deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub stream_key_problem: Field<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_request_leaves_out_absent_fields() {
        let req = ControlRequest { cmd: ControlCmd::Next, index: None, reason: None, id: None };

        let text = serde_json::to_string(&req).unwrap();

        assert_eq!(text, r#"{"cmd":"next"}"#);
    }

    #[test]
    fn control_request_writes_cmd_index_reason_id_in_that_order() {
        let req = ControlRequest { cmd: ControlCmd::RestartEncoder, index: Some(2), reason: Some("r".into()), id: Some("x".into()) };

        let text = serde_json::to_string(&req).unwrap();

        assert_eq!(text, r#"{"cmd":"restart-encoder","index":2,"reason":"r","id":"x"}"#);
    }

    #[test]
    fn reload_request_writes_playlist_index_id_and_nothing_absent() {
        let full = ReloadRequest { playlist: Some("PLb".into()), index: Some(3), id: Some("abc".into()) };

        assert_eq!(serde_json::to_string(&full).unwrap(), r#"{"playlist":"PLb","index":3,"id":"abc"}"#);
        assert_eq!(serde_json::to_string(&ReloadRequest::default()).unwrap(), "{}");
    }

    #[test]
    fn youtube_keeps_whether_a_bot_check_paused_it_and_leaves_it_out_when_absent() {
        let paused: SupervisorStatus = serde_json::from_value(json!({"youtube": {"breaker": "open", "bot": false}})).unwrap();
        let older: SupervisorStatus = serde_json::from_value(json!({"youtube": {"breaker": "open"}})).unwrap();

        let (paused, older) = (serde_json::to_value(&paused).unwrap(), serde_json::to_value(&older).unwrap());

        assert_eq!(paused["youtube"]["bot"], json!(false));
        assert!(older["youtube"].get("bot").is_none(), "{older}");
    }

    #[test]
    fn playlist_id_absent_and_null_differ() {
        let absent: SupervisorStatus = serde_json::from_value(json!({"playing": {"index": 0}})).unwrap();
        let null: SupervisorStatus = serde_json::from_value(json!({"playing": {"playlistId": null}})).unwrap();
        let named: SupervisorStatus = serde_json::from_value(json!({"playing": {"playlistId": "PLa"}})).unwrap();

        assert_eq!(absent.playing.unwrap().playlist_id, None);
        assert_eq!(null.playing.unwrap().playlist_id, Some(None));
        assert_eq!(named.playing.unwrap().playlist_id, Some(Some("PLa".to_string())));
    }

    #[test]
    fn status_ignores_unknown_fields() {
        let s: SupervisorStatus = serde_json::from_value(json!({"state": "playing", "newer": {"x": 1}, "playing": {"later": true}})).unwrap();

        assert_eq!(s.state.as_deref(), Some("playing"));
    }
}
