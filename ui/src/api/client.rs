use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::{
    Api, ApiError,
    types::{
        AutostartStatus, DiscordDefaults, Event, LogFile, SaveResult, SetupReport, Status,
        UpdateStatus, VideoPage, YtdlpUpdate,
    },
};

/// Videos per page in the playlist browser, as the dashboard asks.
const VIDEOS_PER_PAGE: u32 = 25;

/// Every call the dashboard makes, with the path and body admin.js sends.
#[derive(Clone)]
pub struct Client(pub Arc<dyn Api>);

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|_| ApiError::Unreadable)
}

/// Escapes like JavaScript's `encodeURIComponent`.
fn encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Client {
    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        parse(self.0.get_json(path)?)
    }

    fn send(&self, path: &str, body: Option<Value>) -> Result<(), ApiError> {
        self.0.post(path, body).map(|_| ())
    }

    fn send_for<T: DeserializeOwned>(&self, path: &str, body: Option<Value>) -> Result<T, ApiError> {
        parse(self.0.post(path, body)?)
    }

    pub fn status(&self) -> Result<Status, ApiError> {
        self.get("/api/status")
    }

    pub fn events(&self) -> Result<Vec<Event>, ApiError> {
        self.get("/api/events")
    }

    pub fn update_status(&self) -> Result<UpdateStatus, ApiError> {
        self.get("/api/update/status")
    }

    pub fn config(&self) -> Result<Value, ApiError> {
        self.0.get_json("/api/config")
    }

    pub fn save_config(&self, patch: Value) -> Result<SaveResult, ApiError> {
        self.send_for("/api/config", Some(patch))
    }

    pub fn set_stream_key(&self, key: &str) -> Result<(), ApiError> {
        self.send("/api/stream-key", Some(json!({ "key": key })))
    }

    pub fn autostart(&self) -> Result<AutostartStatus, ApiError> {
        self.get("/api/autostart")
    }

    pub fn set_autostart(&self, enabled: bool) -> Result<(), ApiError> {
        self.send("/api/autostart", Some(json!({ "enabled": enabled })))
    }

    pub fn discord_test(&self) -> Result<(), ApiError> {
        self.send("/api/discord/test", None)
    }

    pub fn discord_defaults(&self) -> Result<DiscordDefaults, ApiError> {
        self.get("/api/discord/defaults")
    }

    pub fn update_check(&self) -> Result<UpdateStatus, ApiError> {
        self.send_for("/api/update/check", None)
    }

    pub fn update_apply(&self) -> Result<(), ApiError> {
        self.send("/api/update/apply", None)
    }

    pub fn videos(&self, playlist: usize, page: u32) -> Result<VideoPage, ApiError> {
        self.get(&format!(
            "/api/playlist/videos?playlist={playlist}&page={page}&perPage={VIDEOS_PER_PAGE}"
        ))
    }

    pub fn switch_playlist(
        &self,
        playlist_index: usize,
        video_index: u32,
        video_id: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut body = json!({ "playlistIndex": playlist_index, "videoIndex": video_index });
        if let Some(id) = video_id {
            body["videoId"] = json!(id);
        }
        self.send("/api/playlist/switch", Some(body))
    }

    /// `cmd` is `prev`, `next` or `restart`.
    pub fn player(&self, cmd: &'static str) -> Result<(), ApiError> {
        self.send(&format!("/api/player/{cmd}"), None)
    }

    pub fn jump(&self, index: u32, id: Option<&str>) -> Result<(), ApiError> {
        let mut body = json!({ "index": index });
        if let Some(id) = id {
            body["id"] = json!(id);
        }
        self.send("/api/player/jump", Some(body))
    }

    pub fn stream_start(&self) -> Result<(), ApiError> {
        self.send("/api/stream/start", None)
    }

    pub fn stream_stop(&self) -> Result<(), ApiError> {
        self.send("/api/stream/stop", None)
    }

    /// The saved report, or `None` when no check has run yet.
    pub fn setup_check(&self) -> Result<Option<SetupReport>, ApiError> {
        let value = self.0.get_json("/api/setup-check")?;
        if value.is_null() {
            return Ok(None);
        }
        parse(value).map(Some)
    }

    pub fn run_setup_check(&self, playlist_id: Option<&str>) -> Result<SetupReport, ApiError> {
        let mut body = Map::new();
        if let Some(id) = playlist_id {
            body.insert("playlistId".into(), json!(id));
        }
        self.send_for("/api/setup-check/run", Some(Value::Object(body)))
    }

    pub fn logs(&self) -> Result<Vec<LogFile>, ApiError> {
        self.get("/api/logs")
    }

    pub fn log(&self, source: &str, name: &str) -> Result<String, ApiError> {
        self.0.get_text(&format!(
            "/api/logs/{}/{}",
            encode_component(source),
            encode_component(name)
        ))
    }

    pub fn ytdlp_update(&self) -> Result<YtdlpUpdate, ApiError> {
        self.send_for("/api/yt-dlp/update", None)
    }

    pub fn shutdown(&self) -> Result<(), ApiError> {
        self.send("/api/shutdown", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::fake::FakeApi;
    use serde_json::json;

    fn client() -> (Arc<FakeApi>, Client) {
        let fake = FakeApi::new();
        (fake.clone(), Client(fake))
    }

    #[test]
    fn jump_leaves_out_a_missing_id() {
        let (fake, c) = client();
        fake.reply("POST", "/api/player/jump", Ok(json!({"ok": true})));

        c.jump(4, None).unwrap();

        assert_eq!(fake.posts()[0].body, Some(json!({"index": 4})));
    }

    #[test]
    fn switch_sends_the_dashboards_body() {
        let (fake, c) = client();
        fake.reply("POST", "/api/playlist/switch", Ok(json!({"ok": true})));

        c.switch_playlist(1, 0, Some("abc")).unwrap();

        assert_eq!(
            fake.posts()[0].body,
            Some(json!({"playlistIndex": 1, "videoIndex": 0, "videoId": "abc"}))
        );
    }

    #[test]
    fn videos_asks_for_25_a_page() {
        let (fake, c) = client();
        fake.reply(
            "GET",
            "/api/playlist/videos?playlist=1&page=2&perPage=25",
            Ok(json!({"videos": []})),
        );

        c.videos(1, 2).unwrap();

        assert_eq!(
            fake.requests()[0].path,
            "/api/playlist/videos?playlist=1&page=2&perPage=25"
        );
    }

    #[test]
    fn log_escapes_the_name() {
        let (fake, c) = client();
        fake.text("/api/logs/node/streamloop-2026-11-08.log", Ok("x".into()));
        fake.text("/api/logs/node/a%20b.log", Ok("y".into()));

        assert_eq!(c.log("node", "streamloop-2026-11-08.log").unwrap(), "x");
        assert_eq!(c.log("node", "a b.log").unwrap(), "y");
    }

    #[test]
    fn run_setup_check_sends_the_playlist_only_when_given() {
        let (fake, c) = client();
        fake.reply("POST", "/api/setup-check/run", Ok(json!({})));

        c.run_setup_check(Some("PLx")).unwrap();
        c.run_setup_check(None).unwrap();

        let posts = fake.posts();
        assert_eq!(posts[0].body, Some(json!({"playlistId": "PLx"})));
        assert_eq!(posts[1].body, Some(json!({})));
    }
}
