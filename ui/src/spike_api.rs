//! The slice of the dashboard API the spike reads, as typed structs, over blocking HTTP.

use std::{sync::OnceLock, time::Duration};

use serde::{Deserialize, de::DeserializeOwned};

static BASE: OnceLock<String> = OnceLock::new();

/// Points every request at the backend on `port`; the first call wins.
pub fn use_port(port: u16) {
    BASE.get_or_init(|| format!("http://127.0.0.1:{port}"));
}

fn base() -> &'static str {
    BASE.get_or_init(|| "http://127.0.0.1:7654".into())
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    pub stream: Stream,
    pub supervisor: Option<Supervisor>,
    pub twitch: Option<Twitch>,
    pub uptime_ms: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Stream {
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Supervisor {
    pub playing: Option<Playing>,
    pub encoder: Option<Encoder>,
    pub youtube: Option<Youtube>,
    pub playlist_count: Option<u32>,
}

#[derive(Deserialize, Default, Clone)]
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

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Encoder {
    pub alive: bool,
    pub speed: Option<f64>,
    pub restarts: u32,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Youtube {
    pub breaker: Option<String>,
    pub retry_at: Option<String>,
    pub cookie_problem: Option<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Twitch {
    pub enabled: bool,
    pub channel_live: Option<bool>,
    pub consecutive_mismatches: u32,
    pub mismatch_threshold: u32,
    pub readback: Option<Readback>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Readback {
    pub state: String,
    pub last_check_at: Option<String>,
}

#[derive(Deserialize)]
pub struct Event {
    pub timestamp: String,
    pub message: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdateStatus {
    pub update_available: bool,
    pub latest_version: Option<String>,
    pub current_version: Option<String>,
}

#[derive(Deserialize)]
pub struct Video {
    pub index: u32,
    pub id: Option<String>,
    pub title: Option<String>,
    pub duration: Option<f64>,
}

#[derive(Deserialize)]
struct VideoPage {
    videos: Vec<Video>,
    total: usize,
}

#[derive(Deserialize)]
pub struct Token {
    pub token: String,
}

#[derive(Deserialize)]
pub struct Config {
    pub playlists: Vec<PlaylistRef>,
}

#[derive(Deserialize, Clone)]
pub struct PlaylistRef {
    pub id: String,
    pub name: Option<String>,
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| ureq::AgentBuilder::new().timeout(Duration::from_secs(3)).build())
}

/// The server's own `error` text when it sent one, as the dashboard shows it.
fn error_text(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, resp) => resp
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(String::from))
            .unwrap_or_else(|| format!("HTTP {code}")),
        other => other.to_string(),
    }
}

pub fn get<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    agent()
        .get(&format!("{}{path}", base()))
        .call()
        .map_err(error_text)?
        .into_json()
        .map_err(|e| e.to_string())
}

pub fn post(path: &str, token: &str, body: Option<serde_json::Value>) -> Result<(), String> {
    let req = agent().post(&format!("{}{path}", base())).set("X-API-Token", token);
    match body {
        Some(body) => req.send_json(body),
        None => req.call(),
    }
    .map(|_| ())
    .map_err(error_text)
}

/// Every video of one configured list; the dashboard pages them 25 at a time, the spike keeps them all.
pub fn all_videos(playlist: usize) -> Result<Vec<Video>, String> {
    let mut videos = Vec::new();
    for page in 1.. {
        let p: VideoPage = get(&format!("/api/playlist/videos?playlist={playlist}&page={page}&perPage=100"))?;
        let got = p.videos.len();
        videos.extend(p.videos);
        if got == 0 || videos.len() >= p.total {
            break;
        }
    }
    Ok(videos)
}
