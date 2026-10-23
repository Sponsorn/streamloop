//! Supervisor settings from the `config.json` the Node backend owns.

use serde_json::Value;

#[derive(Clone, PartialEq, Default)]
pub struct Settings {
    pub playlist_id: String,
    pub ingest_url: String,
    pub stream_key: String,
    pub encoder: String,
    pub cache_budget_mb: u64,
    pub read_ahead_seconds: f64,
    pub player_client: String,
    pub overlay: bool,
    pub freeze_seconds: f64,
    pub silence_seconds: f64,
    /// Why `streamKeyProtected` could not be decrypted (another PC or user, a damaged blob).
    pub key_error: Option<String>,
}

/// A missing (or null) key takes the default; a key of the wrong type is an error.
fn field<T>(s: &Value, key: &str, default: T, get: impl Fn(&Value) -> Option<T>) -> Result<T, String> {
    match s.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => get(v).ok_or(format!("config.json: supervisor.{key} has the wrong type")),
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(String::from)
}

pub fn load(json: &str, unprotect: &dyn Fn(&str) -> Result<String, String>) -> Result<Settings, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("config.json is not valid JSON: {e}"))?;
    let playlist_id = v["playlists"][0]["id"].as_str().filter(|id| !id.is_empty()).ok_or("config.json: playlists is empty (need playlists[0].id)")?;
    let s = v.get("supervisor").filter(|s| s.is_object()).ok_or("config.json: no supervisor object")?;
    // An unusable key is not a config error: the supervisor stays up and waits for a working one.
    let (stream_key, key_error) = match field(s, "streamKeyProtected", None, |v| v.as_str().map(|b| Some(b.to_string())))?.map(|b| unprotect(&b)) {
        Some(Ok(key)) => (key, None),
        Some(Err(e)) => (String::new(), Some(format!("config.json: supervisor.streamKeyProtected cannot be read: {e}"))),
        None => (String::new(), None),
    };
    Ok(Settings {
        playlist_id: playlist_id.into(),
        ingest_url: field(s, "ingestUrl", "rtmp://live.twitch.tv/app".into(), text)?,
        stream_key,
        encoder: field(s, "encoder", "h264_nvenc".into(), text)?,
        cache_budget_mb: field(s, "cacheBudgetMb", 300, Value::as_u64)?,
        read_ahead_seconds: field(s, "readAheadSeconds", 20.0, Value::as_f64)?,
        player_client: field(s, "playerClient", "mweb,web_embedded".into(), text)?,
        overlay: field(s, "overlay", true, Value::as_bool)?,
        freeze_seconds: field(s, "freezeSeconds", 20.0, Value::as_f64)?,
        silence_seconds: field(s, "silenceSeconds", 30.0, Value::as_f64)?,
        key_error,
    })
}

pub fn target_url(s: &Settings) -> String {
    format!("{}/{}", s.ingest_url.trim_end_matches('/'), s.stream_key)
}

pub fn redact(text: &str, keys: &[String]) -> String {
    keys.iter().filter(|k| !k.is_empty()).fold(text.to_string(), |t, k| t.replace(k.as_str(), "***"))
}

/// Whether these settings can go on air: Err((event kind, reason)) puts the supervisor in needs-stream-key.
pub fn stream_ready(s: &Settings) -> Result<(), (&'static str, String)> {
    if let Some(e) = &s.key_error {
        Err(("stream-key-unreadable", e.clone()))
    } else if s.stream_key.is_empty() {
        Err(("stream-key-missing", "config.json has no supervisor.streamKeyProtected".into()))
    } else {
        Ok(())
    }
}

/// By hand so a `{:?}` can never print the key.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("playlist_id", &self.playlist_id)
            .field("ingest_url", &self.ingest_url)
            .field("stream_key", &"***")
            .field("encoder", &self.encoder)
            .field("cache_budget_mb", &self.cache_budget_mb)
            .field("read_ahead_seconds", &self.read_ahead_seconds)
            .field("player_client", &self.player_client)
            .field("overlay", &self.overlay)
            .field("freeze_seconds", &self.freeze_seconds)
            .field("silence_seconds", &self.silence_seconds)
            .field("key_error", &self.key_error)
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum Reload {
    Nothing,
    Live,
    Restart,
}

/// Restart outranks Live: a reload with both still applies the live part.
pub fn reload_plan(old: &Settings, new: &Settings) -> Reload {
    if old.ingest_url != new.ingest_url || old.stream_key != new.stream_key || old.encoder != new.encoder {
        Reload::Restart
    } else if old != new {
        Reload::Live
    } else {
        Reload::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_key(_: &str) -> Result<String, String> {
        Err("unprotect must not be called".into())
    }

    fn fake(blob: &str) -> Result<String, String> {
        Ok(format!("live_{blob}"))
    }

    #[test]
    fn load_fills_defaults() {
        let s = load(r#"{"playlists":[{"id":"PLabc"}],"supervisor":{}}"#, &no_key).unwrap();
        assert_eq!(s.playlist_id, "PLabc");
        assert_eq!(s.ingest_url, "rtmp://live.twitch.tv/app");
        assert_eq!(s.stream_key, "");
        assert_eq!(s.encoder, "h264_nvenc");
        assert_eq!(s.cache_budget_mb, 300);
        assert_eq!(s.read_ahead_seconds, 20.0);
        assert_eq!(s.player_client, "mweb,web_embedded");
        assert!(s.overlay);
        assert_eq!((s.freeze_seconds, s.silence_seconds), (20.0, 30.0));
    }

    #[test]
    fn load_reads_camel_case_keys_and_unprotects_the_key() {
        let json = r#"{"playlists":[{"id":"PLx"},{"id":"PLy"}],"supervisor":{"ingestUrl":"rtmp://a/app","streamKeyProtected":"blob",
            "encoder":"libx264","cacheBudgetMb":500,"readAheadSeconds":12.5,"playerClient":"tv","overlay":false,"freezeSeconds":8,"silenceSeconds":9}}"#;
        let s = load(json, &fake).unwrap();
        assert_eq!((s.playlist_id.as_str(), s.ingest_url.as_str(), s.stream_key.as_str(), s.encoder.as_str()), ("PLx", "rtmp://a/app", "live_blob", "libx264"));
        assert_eq!((s.cache_budget_mb, s.read_ahead_seconds, s.player_client.as_str(), s.overlay), (500, 12.5, "tv", false));
        assert_eq!((s.freeze_seconds, s.silence_seconds), (8.0, 9.0));
        assert_eq!(target_url(&s), "rtmp://a/app/live_blob");
    }

    #[test]
    fn load_rejects_bad_config_naming_the_field() {
        let e = load(r#"{"playlists":[{"id":"PLabc"}]}"#, &no_key).unwrap_err();
        assert!(e.contains("supervisor"), "{e}");
        let e = load(r#"{"playlists":[],"supervisor":{}}"#, &no_key).unwrap_err();
        assert!(e.contains("playlists"), "{e}");
        let e = load(r#"{"supervisor":{}}"#, &no_key).unwrap_err();
        assert!(e.contains("playlists"), "{e}");
        let e = load(r#"{"playlists":[{"id":"PLabc"}],"supervisor":{"#, &no_key).unwrap_err();
        assert!(e.contains("config.json") && e.contains("JSON"), "{e}");
        let e = load(r#"{"playlists":[{"id":"PLabc"}],"supervisor":{"cacheBudgetMb":"big"}}"#, &no_key).unwrap_err();
        assert!(e.contains("supervisor.cacheBudgetMb"), "{e}");
    }

    #[test]
    fn redact_replaces_every_occurrence_of_every_key() {
        let keys = vec!["live_123_abc".to_string(), "live_456_new".to_string()];
        assert_eq!(redact("rtmp://x/app/live_123_abc: I/O error (live_123_abc)", &keys), "rtmp://x/app/***: I/O error (***)");
        assert_eq!(redact("old live_123_abc, new live_456_new", &keys), "old ***, new ***");
        assert_eq!(redact("nothing secret here", &keys), "nothing secret here");
        assert_eq!(redact("anything", &[String::new()]), "anything");
    }

    #[test]
    fn an_unreadable_key_means_needs_stream_key_with_the_reason() {
        let json = r#"{"playlists":[{"id":"P"}],"supervisor":{"streamKeyProtected":"b"}}"#;
        let s = load(json, &|_: &str| Err("DPAPI: key not valid for use in specified state".to_string())).unwrap();
        let (kind, reason) = stream_ready(&s).unwrap_err();
        assert_eq!(kind, "stream-key-unreadable");
        assert!(reason.contains("streamKeyProtected") && reason.contains("not valid"), "{reason}");
    }

    #[test]
    fn a_missing_key_means_needs_stream_key() {
        let s = load(r#"{"playlists":[{"id":"P"}],"supervisor":{}}"#, &no_key).unwrap();
        let (kind, reason) = stream_ready(&s).unwrap_err();
        assert_eq!(kind, "stream-key-missing");
        assert!(reason.contains("streamKeyProtected"), "{reason}");
        assert_eq!(stream_ready(&load(r#"{"playlists":[{"id":"P"}],"supervisor":{"streamKeyProtected":"b"}}"#, &fake).unwrap()), Ok(()));
    }

    #[test]
    fn debug_output_hides_the_key() {
        let s = Settings { stream_key: "live_123_SeCrEt".into(), playlist_id: "PLx".into(), ..Settings::default() };
        let d = format!("{s:?}");
        assert!(!d.contains("SeCrEt") && d.contains("***") && d.contains("PLx"), "{d}");
    }

    #[test]
    fn reload_plan_sorts_changes() {
        let old = load(r#"{"playlists":[{"id":"PLa"}],"supervisor":{}}"#, &no_key).unwrap();
        assert_eq!(reload_plan(&old, &old.clone()), Reload::Nothing);
        assert_eq!(reload_plan(&old, &Settings { playlist_id: "PLb".into(), ..old.clone() }), Reload::Live);
        assert_eq!(reload_plan(&old, &Settings { read_ahead_seconds: 10.0, ..old.clone() }), Reload::Live);
        assert_eq!(reload_plan(&old, &Settings { encoder: "libx264".into(), ..old.clone() }), Reload::Restart);
        assert_eq!(reload_plan(&old, &Settings { stream_key: "k".into(), playlist_id: "PLb".into(), ..old.clone() }), Reload::Restart);
    }
}
