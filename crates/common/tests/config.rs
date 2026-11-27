//! config.test.ts ported case by case, then the golden fixtures recorded from the Node loader.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value as Json};
use streamloop_common::config::{self, ordered::Value, Config, ConfigError, Discord, DiscordEvents, DiscordTemplates, Playlist, Supervisor};
use streamloop_common::encoder::Encoder;

/// What a 3.0.x install holds, typed out: key order counts, a reordered file is rewritten on every start.
const WRITTEN_BY_3_0: &str = r#"{
  "port": 7700,
  "playlists": [
    {
      "id": "PLone",
      "name": "One"
    },
    {
      "id": "PLtwo"
    }
  ],
  "discord": {
    "webhookUrl": "https://discord.example/hook",
    "botName": "Bot",
    "avatarUrl": "",
    "rolePing": "123",
    "events": {
      "error": true,
      "skip": false,
      "recovery": true,
      "critical": true,
      "resume": true,
      "streamDrop": true,
      "streamRestart": true,
      "outputFrozen": true,
      "twitchMismatch": false,
      "twitchRestart": true
    },
    "templates": {
      "error": "custom {errorCode}",
      "skip": "custom {reason}",
      "recovery": "custom {step}",
      "critical": "custom {message}",
      "resume": "custom {videoId}",
      "streamDrop": "custom {attempt}",
      "streamRestart": "custom {attempts}",
      "outputFrozen": "custom {kind}",
      "twitchMismatch": "custom {channel}",
      "twitchRestart": "custom {channel}"
    }
  },
  "supervisor": {
    "ingestUrl": "rtmp://ingest.example/app",
    "streamKeyProtected": "AQAAANCMnd8BFdERjHoAwE_fake_blob",
    "encoder": "h264_amf",
    "preferredEncoder": "h264_nvenc",
    "cacheBudgetMb": 8192,
    "readAheadSeconds": 12.5,
    "playerClient": "tv",
    "overlay": false,
    "freezeSeconds": 25,
    "silenceSeconds": 45,
    "futureKnob": 7
  },
  "autoUpdateCheck": false,
  "updateCheckIntervalMs": 3600000,
  "autoUpdateYtdlp": false,
  "autoOpenAdmin": false,
  "autoStartStream": true,
  "twitchClientId": "client-id",
  "twitchClientSecret": "client-secret",
  "twitchChannel": "channel",
  "twitchLivenessEnabled": true,
  "twitchPollIntervalMs": 30000,
  "ytdlCookiesFromBrowser": "firefox"
}"#;

/// A fresh, empty folder for one test.
fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sl-common-config-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `config.json` in a fresh folder holding `json` written compact.
fn config_file(name: &str, json: &Json) -> PathBuf {
    let path = dir(name).join("config.json");
    std::fs::write(&path, json.to_string()).unwrap();
    path
}

fn on_disk(path: &Path) -> Json {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn base() -> Json {
    json!({"playlists": [{"id": "PLtest123"}]})
}

fn with(extra: Json) -> Json {
    let mut v = base();
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn playlist(id: &str) -> Playlist {
    Playlist { id: id.into(), name: None }
}

fn repo_example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.json")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config")
}

/// The field paths an error names: the part before each `: `.
fn paths(e: &ConfigError) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for part in e.to_string().split("; ") {
        let path = part.split(": ").next().unwrap().to_string();
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

// loadConfig on a fresh install

#[test]
fn creates_config_json_from_the_example_next_to_it_when_it_is_missing() {
    // Arrange
    let d = dir("fresh-install");
    std::fs::write(d.join("config.example.json"), r#"{"playlists":[{"id":"PLxxxxxxxxxxxxxxxx"}]}"#).unwrap();
    let path = d.join("config.json");

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.playlists, [playlist("PLxxxxxxxxxxxxxxxx")]);
    assert!(config::is_first_run(&cfg));
    assert_eq!(on_disk(&path)["playlists"], json!([{"id": "PLxxxxxxxxxxxxxxxx"}]));
}

#[test]
fn never_replaces_an_existing_config_json_with_the_example() {
    // Arrange
    let d = dir("fresh-install-existing");
    std::fs::write(d.join("config.example.json"), r#"{"playlists":[{"id":"PLxxxxxxxxxxxxxxxx"}]}"#).unwrap();
    let path = d.join("config.json");
    std::fs::write(&path, json!({"playlists": [{"id": "PLmine"}], "supervisor": {"streamKeyProtected": "BLOB"}}).to_string()).unwrap();

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.playlists, [playlist("PLmine")]);
    let disk = on_disk(&path);
    assert_eq!(disk["playlists"], json!([{"id": "PLmine"}]));
    assert_eq!(disk["supervisor"]["streamKeyProtected"], "BLOB");
}

// loadConfig

#[test]
fn loads_valid_config_with_playlists_array() {
    // Arrange
    let path = config_file("valid", &json!({"playlists": [{"id": "PLtest123"}], "port": 4000}));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.playlists, [playlist("PLtest123")]);
    assert_eq!(cfg.port, 4000);
}

#[test]
fn loads_config_with_multiple_playlists_and_names() {
    // Arrange
    let path = config_file("multiple", &json!({"playlists": [{"id": "PL1", "name": "First"}, {"id": "PL2"}]}));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.playlists.len(), 2);
    assert_eq!(cfg.playlists[0], Playlist { id: "PL1".into(), name: Some("First".into()) });
    assert_eq!(cfg.playlists[1], playlist("PL2"));
}

#[test]
fn migrates_old_playlist_id_format_to_playlists_array() {
    // Arrange
    let path = config_file("legacy-playlist-id", &json!({"playlistId": "PLlegacy123"}));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.playlists, [playlist("PLlegacy123")]);
    assert!(on_disk(&path).get("playlistId").is_none());
}

#[test]
fn applies_defaults_for_optional_fields() {
    // Arrange
    let path = config_file("defaults", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.port, 7654);
    assert_eq!(cfg.discord.webhook_url, "");
}

#[test]
fn errors_on_missing_required_fields() {
    // Arrange
    let path = config_file("missing-required", &json!({"port": 7654}));

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert_eq!(paths(&e), ["playlists"]);
}

#[test]
fn errors_on_a_file_that_does_not_exist() {
    // Arrange
    let path = dir("nonexistent").join("nested").join("config.json");

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert!(matches!(e, ConfigError::File { .. }), "{e}");
}

#[test]
fn errors_on_an_empty_playlists_array() {
    // Arrange
    let path = config_file("empty-playlists", &json!({"playlists": []}));

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert_eq!(paths(&e), ["playlists"]);
}

#[test]
fn opens_the_admin_panel_on_boot_unless_auto_open_admin_is_false() {
    // Arrange
    let by_default = config_file("admin-default", &base());
    let disabled = config_file("admin-disabled", &with(json!({"autoOpenAdmin": false})));

    // Act
    let by_default = config::load(&by_default).unwrap();
    let disabled = config::load(&disabled).unwrap();

    // Assert
    assert!(by_default.auto_open_admin);
    assert!(!disabled.auto_open_admin);
}

#[test]
fn ytdl_cookies_from_browser_defaults_to_empty() {
    // Arrange
    let path = config_file("cookies-default", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.ytdl_cookies_from_browser, "");
}

#[test]
fn preserves_ytdl_cookies_from_browser_when_set() {
    // Arrange
    let path = config_file("cookies-set", &with(json!({"ytdlCookiesFromBrowser": "brave:Profile 1"})));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.ytdl_cookies_from_browser, "brave:Profile 1");
}

// isFirstRun

#[test]
fn is_first_run_for_the_placeholder_playlist() {
    // Arrange
    let path = config_file("first-run", &json!({"playlists": [{"id": "PLxxxxxxxxxxxxxxxx"}]}));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert!(config::is_first_run(&cfg));
}

#[test]
fn is_not_first_run_for_a_real_playlist() {
    // Arrange
    let path = config_file("real-playlist", &json!({"playlists": [{"id": "PLrealPlaylist"}]}));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert!(!config::is_first_run(&cfg));
}

#[test]
fn is_not_first_run_with_several_playlists_even_if_the_first_has_xxxxx() {
    // Arrange
    let path = config_file("several-playlists", &json!({"playlists": [{"id": "PLxxxxxxxxxxxxxxxx"}, {"id": "PLreal"}]}));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert!(!config::is_first_run(&cfg));
}

// discord config migration

#[test]
fn migrates_old_flat_discord_webhook_url_to_nested_discord_webhook_url() {
    // Arrange
    let path = config_file("flat-webhook", &with(json!({"discordWebhookUrl": "https://discord.com/api/webhooks/old"})));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.discord.webhook_url, "https://discord.com/api/webhooks/old");
    assert!(on_disk(&path).get("discordWebhookUrl").is_none());
}

#[test]
fn does_not_overwrite_an_existing_discord_object_during_migration() {
    // Arrange
    let path = config_file("nested-webhook", &with(json!({"discord": {"webhookUrl": "https://discord.com/api/webhooks/new"}})));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.discord.webhook_url, "https://discord.com/api/webhooks/new");
}

// discord config defaults

#[test]
fn applies_default_event_toggles_all_enabled() {
    // Arrange
    let path = config_file("default-toggles", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    let e = &cfg.discord.events;
    assert!(e.error && e.skip && e.recovery && e.critical && e.resume && e.output_frozen);
    assert!(on_disk(&path)["discord"]["events"].get("obsDisconnect").is_none());
}

#[test]
fn applies_default_templates() {
    // Arrange
    let path = config_file("default-templates", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    let t = &cfg.discord.templates;
    assert!(t.error.contains("{errorCode}"));
    assert!(t.skip.contains("{reason}"));
    assert!(t.recovery.contains("{step}"));
    assert!(t.critical.contains("{message}"));
    assert!(t.resume.contains("{videoIndex}"));
    assert!(t.output_frozen.contains("{kind}"));
    assert!(t.output_frozen.contains("{videoIndex}"));
}

#[test]
fn applies_default_empty_strings_for_bot_identity() {
    // Arrange
    let path = config_file("bot-identity", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg.discord.bot_name, "");
    assert_eq!(cfg.discord.avatar_url, "");
    assert_eq!(cfg.discord.role_ping, "");
}

#[test]
fn preserves_partial_event_overrides_without_clobbering_others() {
    // Arrange
    let path = config_file("partial-events", &with(json!({"discord": {"webhookUrl": "https://discord.com/api/webhooks/test", "events": {"error": false}}})));

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert!(!cfg.discord.events.error);
    assert!(cfg.discord.events.skip);
    assert!(cfg.discord.events.critical);
}

// supervisor config

#[test]
fn fills_supervisor_defaults_when_the_section_is_missing() {
    // Arrange
    let path = config_file("supervisor-defaults", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(
        cfg.supervisor,
        Supervisor {
            ingest_url: "rtmp://live.twitch.tv/app".into(),
            stream_key_protected: "".into(),
            encoder: Encoder::Nvenc,
            preferred_encoder: None,
            cache_budget_mb: 300,
            read_ahead_seconds: 20.0,
            player_client: "mweb,web_embedded".into(),
            overlay: true,
            freeze_seconds: 20.0,
            silence_seconds: 30.0,
            extra: vec![],
        }
    );
}

#[test]
fn keeps_the_supervisor_section_across_load_and_write_back() {
    // Arrange
    let path = config_file("supervisor-kept", &with(json!({"supervisor": {"streamKeyProtected": "blob", "encoder": "libx264"}})));

    // Act
    config::load(&path).unwrap();

    // Assert
    let disk = on_disk(&path);
    assert_eq!(disk["supervisor"]["streamKeyProtected"], "blob");
    assert_eq!(disk["supervisor"]["encoder"], "libx264");
}

#[test]
fn drops_stale_2x_obs_mpv_and_recovery_keys_on_write_back() {
    // Arrange
    let stale = ["obsBrowserSourceName", "obsWebsocketUrl", "obsAutoStream", "mpvGeometry", "ytdlPlayerClient", "heartbeatIntervalMs", "stateFilePath"];
    let path = config_file(
        "stale-keys",
        &with(json!({
            "obsBrowserSourceName": "Source", "obsWebsocketUrl": "ws://x", "obsAutoStream": true,
            "mpvGeometry": "1x1+0+0", "ytdlPlayerClient": "tv", "heartbeatIntervalMs": 1000, "stateFilePath": "./state.json"
        })),
    );

    // Act
    config::load(&path).unwrap();

    // Assert
    let disk = on_disk(&path);
    for k in stale {
        assert!(disk.get(k).is_none(), "{k} survived");
    }
}

/// The supervisor section with one value below its minimum must be refused, naming that field.
fn below_minimum_is_refused(key: &str, bad: Json) {
    // Arrange
    let path = config_file(&format!("below-{key}"), &with(json!({"supervisor": {key: bad}})));

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert_eq!(paths(&e), [format!("supervisor.{key}")]);
}

#[test]
fn rejects_read_ahead_seconds_below_its_minimum_naming_the_field() {
    below_minimum_is_refused("readAheadSeconds", json!(1.9));
}

#[test]
fn rejects_freeze_seconds_below_its_minimum_naming_the_field() {
    below_minimum_is_refused("freezeSeconds", json!(4));
}

#[test]
fn rejects_silence_seconds_below_its_minimum_naming_the_field() {
    below_minimum_is_refused("silenceSeconds", json!(4));
}

#[test]
fn rejects_cache_budget_mb_below_its_minimum_naming_the_field() {
    below_minimum_is_refused("cacheBudgetMb", json!(49));
}

#[test]
fn rejects_an_unknown_encoder() {
    // Arrange
    let path = config_file("unknown-encoder", &with(json!({"supervisor": {"encoder": "h265"}})));

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert_eq!(paths(&e), ["supervisor.encoder"]);
}

#[test]
fn keeps_stream_key_protected_when_saving_a_partial_supervisor_patch() {
    // Arrange
    let path = config_file("partial-patch", &with(json!({"supervisor": {"streamKeyProtected": "blob"}})));

    // Act
    let saved = config::save(&json!({"supervisor": {"encoder": "h264_amf"}}), &path).unwrap();

    // Assert
    assert_eq!(saved.supervisor.stream_key_protected, "blob");
    assert_eq!(saved.supervisor.encoder, Encoder::Amf);
    assert_eq!(on_disk(&path)["supervisor"]["streamKeyProtected"], "blob");
}

#[test]
fn rejects_a_fractional_cache_budget_mb() {
    // Arrange
    let path = config_file("fractional-cache", &with(json!({"supervisor": {"cacheBudgetMb": 300.5}})));

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert_eq!(paths(&e), ["supervisor.cacheBudgetMb"]);
}

#[test]
fn keeps_stream_key_protected_when_a_save_carries_an_empty_one() {
    // Arrange
    let path = config_file("empty-key-save", &with(json!({"supervisor": {"streamKeyProtected": "blob"}})));

    // Act
    let saved = config::save(&json!({"supervisor": {"streamKeyProtected": "", "encoder": "libx264"}}), &path).unwrap();

    // Assert
    assert_eq!(saved.supervisor.stream_key_protected, "blob");
    assert_eq!(saved.supervisor.encoder, Encoder::Libx264);
}

#[test]
fn replaces_stream_key_protected_when_a_save_carries_a_non_empty_one() {
    // Arrange
    let path = config_file("new-key-save", &with(json!({"supervisor": {"streamKeyProtected": "blob"}})));

    // Act
    let saved = config::save(&json!({"supervisor": {"streamKeyProtected": "new"}}), &path).unwrap();

    // Assert
    assert_eq!(saved.supervisor.stream_key_protected, "new");
    assert_eq!(on_disk(&path)["supervisor"]["streamKeyProtected"], "new");
}

#[test]
fn saves_a_blank_ingest_url_or_player_client_as_the_default() {
    // Arrange
    let path = config_file("blank-save", &with(json!({"supervisor": {"ingestUrl": "rtmp://custom/app", "playerClient": "tv"}})));

    // Act
    let saved = config::save(&json!({"supervisor": {"ingestUrl": "", "playerClient": " "}}), &path).unwrap();

    // Assert
    assert_eq!(saved.supervisor.ingest_url, "rtmp://live.twitch.tv/app");
    assert_eq!(saved.supervisor.player_client, "mweb,web_embedded");
}

#[test]
fn leaves_the_patch_it_was_given_unchanged() {
    // Arrange
    let path = config_file("patch-unchanged", &with(json!({"discord": {"botName": "Old"}, "supervisor": {"streamKeyProtected": "blob"}})));
    let input = json!({"discord": {"webhookUrl": "https://example.com/hook"}, "supervisor": {"encoder": "libx264", "streamKeyProtected": ""}});
    let before = input.clone();

    // Act
    config::save(&input, &path).unwrap();

    // Assert
    assert_eq!(input, before);
}

#[test]
fn keeps_unknown_supervisor_keys_through_load_and_save() {
    // Arrange
    let path = config_file("unknown-supervisor-keys", &with(json!({"supervisor": {"futureKnob": 7}})));

    // Act
    config::load(&path).unwrap();
    let after_load = on_disk(&path)["supervisor"]["futureKnob"].clone();
    config::save(&json!({"port": 7000}), &path).unwrap();
    let after_save = on_disk(&path)["supervisor"]["futureKnob"].clone();

    // Assert
    assert_eq!(after_load, 7);
    assert_eq!(after_save, 7);
}

// a config.json written by 3.0.x

#[test]
fn a_3_0_x_config_loads_with_every_value_as_written_and_leaves_the_file_untouched() {
    // Arrange
    let path = dir("written-by-3-0").join("config.json");
    std::fs::write(&path, WRITTEN_BY_3_0).unwrap();

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    let expected = Config {
        port: 7700,
        playlists: vec![Playlist { id: "PLone".into(), name: Some("One".into()) }, playlist("PLtwo")],
        discord: Discord {
            webhook_url: "https://discord.example/hook".into(),
            bot_name: "Bot".into(),
            avatar_url: "".into(),
            role_ping: "123".into(),
            events: DiscordEvents {
                error: true,
                skip: false,
                recovery: true,
                critical: true,
                resume: true,
                stream_drop: true,
                stream_restart: true,
                output_frozen: true,
                twitch_mismatch: false,
                twitch_restart: true,
            },
            templates: DiscordTemplates {
                error: "custom {errorCode}".into(),
                skip: "custom {reason}".into(),
                recovery: "custom {step}".into(),
                critical: "custom {message}".into(),
                resume: "custom {videoId}".into(),
                stream_drop: "custom {attempt}".into(),
                stream_restart: "custom {attempts}".into(),
                output_frozen: "custom {kind}".into(),
                twitch_mismatch: "custom {channel}".into(),
                twitch_restart: "custom {channel}".into(),
            },
        },
        supervisor: Supervisor {
            ingest_url: "rtmp://ingest.example/app".into(),
            stream_key_protected: "AQAAANCMnd8BFdERjHoAwE_fake_blob".into(),
            encoder: Encoder::Amf,
            preferred_encoder: Some(Encoder::Nvenc),
            cache_budget_mb: 8192,
            read_ahead_seconds: 12.5,
            player_client: "tv".into(),
            overlay: false,
            freeze_seconds: 25.0,
            silence_seconds: 45.0,
            extra: vec![("futureKnob".into(), Value::Number(7.0))],
        },
        auto_update_check: false,
        update_check_interval_ms: 3_600_000,
        auto_update_ytdlp: false,
        auto_open_admin: false,
        auto_start_stream: true,
        twitch_client_id: "client-id".into(),
        twitch_client_secret: "client-secret".into(),
        twitch_channel: "channel".into(),
        twitch_liveness_enabled: true,
        twitch_poll_interval_ms: 30_000,
        ytdl_cookies_from_browser: "firefox".into(),
    };
    assert_eq!(cfg, expected);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), WRITTEN_BY_3_0);
}

#[test]
fn a_3_0_x_config_changes_only_the_saved_field_on_a_save() {
    // Arrange
    let path = dir("written-by-3-0-save").join("config.json");
    std::fs::write(&path, WRITTEN_BY_3_0).unwrap();

    // Act
    config::save(&json!({"port": 7000}), &path).unwrap();

    // Assert
    assert_eq!(std::fs::read_to_string(&path).unwrap(), WRITTEN_BY_3_0.replacen("\"port\": 7700", "\"port\": 7000", 1));
}

/// A config holding the example's playlists plus `sections` loads equal to the repo's config.example.json.
fn sections_fill_with_the_example(name: &str, sections: Json) {
    // Arrange
    let example = config::read(&repo_example()).unwrap();
    let mut json = json!({"playlists": [{"id": "PLxxxxxxxxxxxxxxxx"}]});
    for (k, v) in sections.as_object().unwrap() {
        json[k] = v.clone();
    }
    let path = config_file(name, &json);

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert_eq!(cfg, example);
}

#[test]
fn fills_absent_sections_with_the_values_in_config_example_json() {
    sections_fill_with_the_example("absent-sections", json!({}));
}

#[test]
fn fills_empty_sections_with_the_values_in_config_example_json() {
    sections_fill_with_the_example("empty-sections", json!({"discord": {}, "supervisor": {}}));
}

#[test]
fn fills_empty_nested_sections_with_the_values_in_config_example_json() {
    sections_fill_with_the_example("empty-nested-sections", json!({"discord": {"events": {}, "templates": {}}}));
}

// old discord keys

#[test]
fn drops_old_discord_keys_obs_disconnect_and_obs_reconnect() {
    // Arrange
    let path = config_file("old-discord-keys", &with(json!({"discord": {"events": {"obsDisconnect": true}, "templates": {"obsReconnect": "x"}}})));

    // Act
    config::load(&path).unwrap();

    // Assert
    let disk = on_disk(&path);
    assert!(disk["discord"]["events"].get("obsDisconnect").is_none());
    assert!(disk["discord"]["templates"].get("obsReconnect").is_none());
}

// beyond config.test.ts

#[test]
fn matches_every_config_fixture() {
    // Arrange
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixture_dir()).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    let mut passed = 0;

    for file in &files {
        let case: Json = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        let name = case["name"].as_str().unwrap();
        let d = dir(&format!("fixture-{passed}"));
        let path = d.join("config.json");
        let before = case["before"].as_str();
        if let Some(text) = before {
            std::fs::write(&path, text).unwrap();
        }
        if let Some(example) = case["example"].as_str() {
            std::fs::write(d.join("config.example.json"), example).unwrap();
        }

        // Act
        let result = match case.get("patch") {
            Some(patch) => config::save(patch, &path),
            None => config::load(&path),
        };

        // Assert
        let text = std::fs::read_to_string(&path).ok();
        match case.get("error").and_then(Json::as_str) {
            Some(error) => {
                let e = result.err().unwrap_or_else(|| panic!("{name}: loaded, Node refused it with {error}"));
                match serde_json::from_str::<Vec<Json>>(error) {
                    Ok(issues) => {
                        let mut expected: Vec<String> = vec![];
                        for issue in issues {
                            let parts: Vec<String> = issue["path"].as_array().unwrap().iter().map(|p| p.as_str().map(String::from).unwrap_or_else(|| p.to_string())).collect();
                            let path = parts.join(".");
                            if !expected.contains(&path) {
                                expected.push(path);
                            }
                        }
                        assert_eq!(paths(&e), expected, "{name}: {e}");
                    }
                    Err(_) => assert!(matches!(e, ConfigError::File { .. }), "{name}: Node failed on the file itself ({error}), ours named fields: {e}"),
                }
                assert_eq!(text.as_deref(), before, "{name}: a refused call changed the file");
            }
            None => {
                result.unwrap_or_else(|e| panic!("{name}: {e}"));
                let expected = case["after"].as_str().or(before);
                assert_eq!(text.as_deref(), expected, "{name}");
            }
        }
        passed += 1;
    }

    assert_eq!(passed, 66);
}

#[test]
fn a_byte_order_mark_is_skipped() {
    // Arrange
    let path = dir("bom").join("config.json");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(br#"{"playlists":[{"id":"PLbom"}]}"#);
    std::fs::write(&path, &bytes).unwrap();

    // Act
    let read = config::read(&path).unwrap();
    let loaded = config::load(&path).unwrap();

    // Assert
    assert_eq!(read.playlists, [playlist("PLbom")]);
    assert_eq!(loaded.playlists, [playlist("PLbom")]);
    let written = std::fs::read(&path).unwrap();
    assert!(written.starts_with(b"{\n  \"port\": 7654,"), "{}", String::from_utf8_lossy(&written));
}

#[test]
fn whole_numbers_are_written_without_a_fraction() {
    // Arrange
    let whole = config_file("whole-number", &with(json!({"supervisor": {"readAheadSeconds": 20}})));
    let fraction = config_file("fraction", &with(json!({"supervisor": {"readAheadSeconds": 12.5}})));

    // Act
    config::load(&whole).unwrap();
    config::load(&fraction).unwrap();

    // Assert
    assert!(std::fs::read_to_string(&whole).unwrap().contains("\n    \"readAheadSeconds\": 20,\n"));
    assert!(std::fs::read_to_string(&fraction).unwrap().contains("\n    \"readAheadSeconds\": 12.5,\n"));
}

#[test]
fn a_3_0_5_config_gains_auto_start_stream_in_one_write() {
    // Arrange: what 3.0.5 wrote after copying the full example on a fresh install, before the key existed.
    let case: Json = serde_json::from_str(&std::fs::read_to_string(fixture_dir().join("creates-config-json-from-the-full-example.json")).unwrap()).unwrap();
    let written = case["after"].as_str().unwrap();
    let key_line = line_holding(written, "\"autoStartStream\"");
    let before = written.replacen(&key_line, "", 1);
    assert!(!before.contains("autoStartStream"));
    let path = dir("written-by-3-0-5").join("config.json");
    std::fs::write(&path, &before).unwrap();

    // Act
    let first = config::load(&path).unwrap();
    let after_first = std::fs::read_to_string(&path).unwrap();
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(stamp).unwrap();
    config::load(&path).unwrap();

    // Assert
    assert!(first.auto_start_stream);
    assert_eq!(after_first, written);
    assert!(after_first.contains("\"autoOpenAdmin\": true,
  \"autoStartStream\": true,
  \"twitchClientId\""));
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
}

#[test]
fn a_config_written_by_3_0_x_with_auto_start_stream_loads_without_a_write() {
    // Arrange: the full example as a current install holds it.
    let case: Json = serde_json::from_str(&std::fs::read_to_string(fixture_dir().join("creates-config-json-from-the-full-example.json")).unwrap()).unwrap();
    let text = case["after"].as_str().unwrap();
    let path = dir("written-with-auto-start").join("config.json");
    std::fs::write(&path, text).unwrap();
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(stamp).unwrap();

    // Act
    config::load(&path).unwrap();

    // Assert
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn a_saved_auto_start_stream_false_survives_a_load() {
    // Arrange
    let path = config_file("auto-start-false", &with(json!({"autoStartStream": false})));

    // Act
    let cfg = config::load(&path).unwrap();
    let again = config::load(&path).unwrap();

    // Assert
    assert!(!cfg.auto_start_stream);
    assert!(!again.auto_start_stream);
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"autoStartStream\": false"));
}

#[test]
fn auto_start_stream_defaults_to_true() {
    // Arrange
    let path = config_file("auto-start-default", &base());

    // Act
    let cfg = config::load(&path).unwrap();

    // Assert
    assert!(cfg.auto_start_stream);
}

/// The whole line of `text` holding `needle`, with its leading indent and trailing newline.
fn line_holding(text: &str, needle: &str) -> String {
    text.split_inclusive('\n').find(|l| l.contains(needle)).unwrap().to_string()
}

#[test]
fn read_never_writes_or_creates_the_file() {
    // Arrange
    let partial = config_file("read-partial", &base());
    let partial_text = std::fs::read_to_string(&partial).unwrap();
    let missing_dir = dir("read-missing");
    std::fs::write(missing_dir.join("config.example.json"), r#"{"playlists":[{"id":"PLxxxxxxxxxxxxxxxx"}]}"#).unwrap();
    let missing = missing_dir.join("config.json");

    // Act
    let read = config::read(&partial).unwrap();
    let refused = config::read(&missing);

    // Assert
    assert_eq!(read.port, 7654);
    assert_eq!(std::fs::read_to_string(&partial).unwrap(), partial_text);
    assert!(matches!(refused, Err(ConfigError::File { .. })));
    assert!(!missing.exists(), "read created config.json from the example");
}

#[test]
fn an_error_names_every_failing_field_as_path_colon_message_joined_with_semicolons() {
    // Arrange
    let path = config_file("two-errors", &json!({"playlists": [{"id": ""}], "port": 0, "supervisor": {"freezeSeconds": 1}}));

    // Act
    let e = config::load(&path).unwrap_err();

    // Assert
    assert_eq!(paths(&e), ["port", "playlists.0.id", "supervisor.freezeSeconds"]);
    assert_eq!(e.to_string().matches("; ").count(), 2, "{e}");
}

#[test]
fn debug_output_hides_the_secrets() {
    // Arrange
    let path = config_file(
        "debug-secrets",
        &with(json!({"twitchClientSecret": "TwItChSeCrEt", "discord": {"webhookUrl": "https://discord.example/WeBhOoK"}, "supervisor": {"streamKeyProtected": "BlObDaTa"}})),
    );
    let cfg = config::read(&path).unwrap();

    // Act
    let shown = format!("{cfg:?}");

    // Assert
    for secret in ["TwItChSeCrEt", "WeBhOoK", "BlObDaTa"] {
        assert!(!shown.contains(secret), "{secret} in {shown}");
    }
    assert!(shown.contains("PLtest123"), "{shown}");
}

// read_port: the app's read of the port

#[test]
fn reads_the_port_without_writing_config_json() {
    // Arrange: a compact file the loader would write back in schema order
    let path = config_file("read-port", &with(json!({"port": 8123})));
    let before = std::fs::read(&path).unwrap();
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::File::options().write(true).open(&path).unwrap().set_modified(stamp).unwrap();

    // Act
    let port = config::read_port(&path);

    // Assert
    assert_eq!(port, Some(8123));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), stamp);
}

#[test]
fn reads_the_port_after_a_byte_order_mark() {
    // Arrange: Notepad writes one
    let path = dir("read-port-bom").join("config.json");
    std::fs::write(&path, b"\xEF\xBB\xBF{\"port\": 7700}").unwrap();

    // Act
    let port = config::read_port(&path);

    // Assert
    assert_eq!(port, Some(7700));
}

#[test]
fn reads_a_port_written_as_a_whole_float_as_the_schema_does() {
    // Arrange
    let path = dir("read-port-float").join("config.json");
    std::fs::write(&path, "{\"port\": 7655.0}").unwrap();

    // Act
    let port = config::read_port(&path);

    // Assert
    assert_eq!(port, Some(7655));
}

#[test]
fn reads_no_port_from_a_missing_broken_or_portless_file() {
    // Arrange
    let folder = dir("read-port-none");
    let cases = [
        ("missing.json", None),
        ("broken.json", Some("{\"port\": 81")),
        ("portless.json", Some("{\"playlists\": []}")),
        ("zero.json", Some("{\"port\": 0}")),
        ("too-big.json", Some("{\"port\": 70000}")),
        ("fraction.json", Some("{\"port\": 7654.5}")),
        ("text.json", Some("{\"port\": \"7654\"}")),
    ];
    for (name, text) in cases {
        if let Some(text) = text {
            std::fs::write(folder.join(name), text).unwrap();
        }
    }

    // Act
    let ports: Vec<Option<u16>> = cases.iter().map(|(name, _)| config::read_port(&folder.join(name))).collect();

    // Assert
    assert_eq!(ports, vec![None; cases.len()]);
    assert!(!folder.join("missing.json").exists());
}
