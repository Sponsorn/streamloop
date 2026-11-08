//! Supervisor settings from the `config.json` the control process owns.

use std::path::Path;

use streamloop_common::config::ConfigError;

#[derive(Clone, PartialEq, Default)]
pub struct Settings {
    /// The list a start or reload asks for; the others in `playlist_ids` join it in the ring.
    pub playlist_id: String,
    /// Every configured playlist, in the config's order, each once.
    pub playlist_ids: Vec<String>,
    pub ingest_url: String,
    pub stream_key: String,
    pub encoder: String,
    pub cache_budget_mb: u64,
    pub read_ahead_seconds: f64,
    pub player_client: String,
    pub overlay: bool,
    pub freeze_seconds: f64,
    pub silence_seconds: f64,
    /// yt-dlp's `--cookies-from-browser` value (top-level `ytdlCookiesFromBrowser`); empty means none.
    pub cookies_from_browser: String,
    /// Why `streamKeyProtected` could not be decrypted (another PC or user, a damaged blob).
    pub key_error: Option<String>,
}

/// Reads `config.json` through the same schema as the control process, never writing it.
pub fn load(path: &Path, unprotect: &dyn Fn(&str) -> Result<String, String>) -> Result<Settings, String> {
    let c = streamloop_common::config::read(path).map_err(|e| refusal(&e))?;
    let mut playlist_ids: Vec<String> = vec![];
    for p in &c.playlists {
        if !playlist_ids.contains(&p.id) {
            playlist_ids.push(p.id.clone());
        }
    }
    let s = &c.supervisor;
    // An unusable key is not a config error: the supervisor stays up and waits for a working one.
    let (stream_key, key_error) = match s.stream_key_protected.as_str() {
        "" => (String::new(), None),
        blob => match unprotect(blob) {
            Ok(key) => (key, None),
            Err(e) => (String::new(), Some(format!("config.json: supervisor.streamKeyProtected cannot be read: {e}"))),
        },
    };
    Ok(Settings {
        // The schema refuses an empty playlists list and an empty id.
        playlist_id: c.playlists[0].id.clone(),
        playlist_ids,
        ingest_url: s.ingest_url.clone(),
        stream_key,
        encoder: s.encoder.as_str().into(),
        cache_budget_mb: s.cache_budget_mb,
        read_ahead_seconds: s.read_ahead_seconds,
        player_client: s.player_client.clone(),
        overlay: s.overlay,
        freeze_seconds: s.freeze_seconds,
        silence_seconds: s.silence_seconds,
        cookies_from_browser: cookies(&c.ytdl_cookies_from_browser)?,
        key_error,
    })
}

/// The `config-refused` reason: an unknown encoder name keeps the words it had before the shared
/// schema, other field problems follow `config.json: `.
fn refusal(e: &ConfigError) -> String {
    match e {
        ConfigError::Invalid(issues) => match issues.as_slice() {
            [only] if only.path == "supervisor.encoder" && only.message.starts_with("unknown encoder") => only.message.clone(),
            _ => format!("config.json: {e}"),
        },
        ConfigError::File { .. } => e.to_string(),
    }
}

/// The dashboard's cookie setting, shared with the control process's playlist fetch. It goes to
/// yt-dlp as one argument.
fn cookies(value: &str) -> Result<String, String> {
    let bad = |why: &str| format!("config.json: ytdlCookiesFromBrowser {why}");
    let s = value.trim();
    if s.chars().count() > 200 {
        return Err(bad("is longer than 200 characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(bad("contains a control character"));
    }
    Ok(s.into())
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
            .field("playlist_ids", &self.playlist_ids)
            .field("ingest_url", &self.ingest_url)
            .field("stream_key", &"***")
            .field("encoder", &self.encoder)
            .field("cache_budget_mb", &self.cache_budget_mb)
            .field("read_ahead_seconds", &self.read_ahead_seconds)
            .field("player_client", &self.player_client)
            .field("overlay", &self.overlay)
            .field("freeze_seconds", &self.freeze_seconds)
            .field("silence_seconds", &self.silence_seconds)
            .field("cookies_from_browser", &self.cookies_from_browser)
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

/// Restart outranks Live: a reload with both still applies the live part. The freeze and silence
/// durations are encoder arguments, so they too wait for the next encoder start.
pub fn reload_plan(old: &Settings, new: &Settings) -> Reload {
    if old.ingest_url != new.ingest_url
        || old.stream_key != new.stream_key
        || old.encoder != new.encoder
        || old.freeze_seconds != new.freeze_seconds
        || old.silence_seconds != new.silence_seconds
    {
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

    /// `load` of a `config.json` holding `json`, in a folder of its own.
    fn load_json(json: &str, unprotect: &dyn Fn(&str) -> Result<String, String>) -> Result<Settings, String> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sl-supervisor-config-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, json).unwrap();
        let r = load(&path, unprotect);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    #[test]
    fn load_fills_defaults() {
        let s = load_json(r#"{"playlists":[{"id":"PLabc"}],"supervisor":{}}"#, &no_key).unwrap();
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
        let s = load_json(json, &fake).unwrap();
        assert_eq!((s.playlist_id.as_str(), s.ingest_url.as_str(), s.stream_key.as_str(), s.encoder.as_str()), ("PLx", "rtmp://a/app", "live_blob", "libx264"));
        assert_eq!((s.cache_budget_mb, s.read_ahead_seconds, s.player_client.as_str(), s.overlay), (500, 12.5, "tv", false));
        assert_eq!((s.freeze_seconds, s.silence_seconds), (8.0, 9.0));
        assert_eq!(target_url(&s), "rtmp://a/app/live_blob");
    }

    #[test]
    fn load_rejects_bad_config_naming_the_field() {
        let e = load_json(r#"{"playlists":[{"id":"PLabc"}],"supervisor":5}"#, &no_key).unwrap_err();
        assert!(e.contains("supervisor"), "{e}");
        let e = load_json(r#"{"playlists":[],"supervisor":{}}"#, &no_key).unwrap_err();
        assert!(e.contains("playlists"), "{e}");
        let e = load_json(r#"{"supervisor":{}}"#, &no_key).unwrap_err();
        assert!(e.contains("playlists"), "{e}");
        let e = load_json(r#"{"playlists":[{"id":"PLabc"}],"supervisor":{"#, &no_key).unwrap_err();
        assert!(e.contains("config.json") && e.contains("JSON"), "{e}");
        let e = load_json(r#"{"playlists":[{"id":"PLabc"}],"supervisor":{"cacheBudgetMb":"big"}}"#, &no_key).unwrap_err();
        assert!(e.contains("supervisor.cacheBudgetMb"), "{e}");
    }

    #[test]
    fn load_rejects_numbers_that_would_stall_the_stream_naming_the_field() {
        let with = |k: &str, v: &str| load_json(&format!(r#"{{"playlists":[{{"id":"P"}}],"supervisor":{{"{k}":{v}}}}}"#), &no_key);
        for (k, bad, good) in [("readAheadSeconds", "1.9", "2"), ("freezeSeconds", "4", "5"), ("silenceSeconds", "0", "5"), ("cacheBudgetMb", "49", "50")] {
            let e = with(k, bad).unwrap_err();
            assert!(e.contains(&format!("supervisor.{k}")) && e.contains("at least"), "{k}: {e}");
            assert!(with(k, good).is_ok(), "{k} = {good} rejected");
        }
        assert!(with("freezeSeconds", "-30").is_err());
        assert!(with("readAheadSeconds", "1e999").is_err(), "a number too big to be finite");
    }

    #[test]
    fn load_reads_the_cookie_browser_trimmed_and_empty_as_none() {
        let with = |v: &str| load_json(&format!(r#"{{"playlists":[{{"id":"P"}}],"supervisor":{{}},"ytdlCookiesFromBrowser":{v}}}"#), &no_key);
        assert_eq!(load_json(r#"{"playlists":[{"id":"P"}],"supervisor":{}}"#, &no_key).unwrap().cookies_from_browser, "");
        assert_eq!(with(r#""  firefox  ""#).unwrap().cookies_from_browser, "firefox");
        assert_eq!(with(r#""   ""#).unwrap().cookies_from_browser, "");
        assert_eq!(with(r#""brave:Profile 1""#).unwrap().cookies_from_browser, "brave:Profile 1");
        for bad in [format!("\"{}\"", "x".repeat(201)), r#""fire\nfox""#.to_string(), "5".to_string()] {
            let e = with(&bad).unwrap_err();
            assert!(e.contains("ytdlCookiesFromBrowser"), "{bad}: {e}");
        }
        assert!(with(&format!("\"{}\"", "x".repeat(200))).is_ok());
    }

    #[test]
    fn a_new_cookie_browser_applies_live() {
        let old = load_json(r#"{"playlists":[{"id":"PLa"}],"supervisor":{}}"#, &no_key).unwrap();
        assert_eq!(reload_plan(&old, &Settings { cookies_from_browser: "firefox".into(), ..old.clone() }), Reload::Live);
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
        let s = load_json(json, &|_: &str| Err("DPAPI: key not valid for use in specified state".to_string())).unwrap();
        let (kind, reason) = stream_ready(&s).unwrap_err();
        assert_eq!(kind, "stream-key-unreadable");
        assert!(reason.contains("streamKeyProtected") && reason.contains("not valid"), "{reason}");
    }

    #[test]
    fn a_missing_key_means_needs_stream_key() {
        let s = load_json(r#"{"playlists":[{"id":"P"}],"supervisor":{}}"#, &no_key).unwrap();
        let (kind, reason) = stream_ready(&s).unwrap_err();
        assert_eq!(kind, "stream-key-missing");
        assert!(reason.contains("streamKeyProtected"), "{reason}");
        assert_eq!(stream_ready(&load_json(r#"{"playlists":[{"id":"P"}],"supervisor":{"streamKeyProtected":"b"}}"#, &fake).unwrap()), Ok(()));
    }

    #[test]
    fn debug_output_hides_the_key() {
        let s = Settings { stream_key: "live_123_SeCrEt".into(), playlist_id: "PLx".into(), ..Settings::default() };
        let d = format!("{s:?}");
        assert!(!d.contains("SeCrEt") && d.contains("***") && d.contains("PLx"), "{d}");
    }

    #[test]
    fn reload_plan_sorts_changes() {
        let old = load_json(r#"{"playlists":[{"id":"PLa"}],"supervisor":{}}"#, &no_key).unwrap();
        assert_eq!(reload_plan(&old, &old.clone()), Reload::Nothing);
        assert_eq!(reload_plan(&old, &Settings { playlist_id: "PLb".into(), ..old.clone() }), Reload::Live);
        assert_eq!(reload_plan(&old, &Settings { read_ahead_seconds: 10.0, ..old.clone() }), Reload::Live);
        assert_eq!(reload_plan(&old, &Settings { encoder: "libx264".into(), ..old.clone() }), Reload::Restart);
        // the detectors are encoder arguments: they change at the next encoder start
        assert_eq!(reload_plan(&old, &Settings { freeze_seconds: 30.0, ..old.clone() }), Reload::Restart);
        assert_eq!(reload_plan(&old, &Settings { silence_seconds: 60.0, ..old.clone() }), Reload::Restart);
        assert_eq!(reload_plan(&old, &Settings { stream_key: "k".into(), playlist_id: "PLb".into(), ..old.clone() }), Reload::Restart);
    }

    #[test]
    fn every_configured_playlist_is_read_in_order() {
        let s = load_json(r#"{"playlists":[{"id":"PLx"},{"id":"PLy","name":"second"},{"id":"PLz"}],"supervisor":{}}"#, &no_key).unwrap();

        assert_eq!(s.playlist_ids, ["PLx", "PLy", "PLz"]);
        assert_eq!(s.playlist_id, "PLx");
    }

    #[test]
    fn a_playlist_listed_twice_counts_once() {
        let s = load_json(r#"{"playlists":[{"id":"PLx"},{"id":"PLy"},{"id":"PLx"}],"supervisor":{}}"#, &no_key).unwrap();

        assert_eq!(s.playlist_ids, ["PLx", "PLy"]);
    }

    #[test]
    fn blank_ingest_url_means_the_default() {
        // Act
        let s = load_json(r#"{"playlists":[{"id":"P"}],"supervisor":{"ingestUrl":"   ","playerClient":""}}"#, &no_key).unwrap();

        // Assert
        assert_eq!(s.ingest_url, "rtmp://live.twitch.tv/app");
        assert_eq!(s.player_client, "mweb,web_embedded");
    }

    #[test]
    fn missing_supervisor_section_means_defaults() {
        // Act
        let s = load_json(r#"{"playlists":[{"id":"P"}]}"#, &no_key).unwrap();

        // Assert
        assert_eq!((s.ingest_url.as_str(), s.encoder.as_str(), s.cache_budget_mb), ("rtmp://live.twitch.tv/app", "h264_nvenc", 300));
        assert_eq!((s.read_ahead_seconds, s.freeze_seconds, s.silence_seconds), (20.0, 20.0, 30.0));
        assert_eq!((s.player_client.as_str(), s.overlay), ("mweb,web_embedded", true));
    }

    #[test]
    fn empty_stream_key_means_no_key() {
        // Act
        let s = load_json(r#"{"playlists":[{"id":"P"}],"supervisor":{"streamKeyProtected":""}}"#, &no_key).unwrap();

        // Assert
        assert_eq!(s.key_error, None);
        assert_eq!(stream_ready(&s).unwrap_err().0, "stream-key-missing");
    }
}
