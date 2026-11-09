//! The Twitch read-back: two frames of the channel's own HLS 5 s apart. The overlay clock sits in a fixed
//! region of the picture, so a region that stands still twice means Twitch is receiving a stale stream.

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Serialize;
use streamloop_common::api::SupervisorStatus;
use tokio::task::JoinHandle;

use crate::clock::Clock;
use crate::http::{Http, HttpRequest};
use crate::runner::{RunSpec, Runner};
use crate::supervisor_client::SupervisorApi;

/// Twitch's public web player client id: the playback token query takes no user credentials.
const TWITCH_WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const GQL_URL: &str = "https://gql.twitch.tv/gql";
const TOKEN_QUERY: &str = "query PlaybackAccessToken($login: String!) { streamPlaybackAccessToken(channelName: $login, \
params: {platform: \"web\", playerBackend: \"mediaplayer\", playerType: \"site\"}) { value signature } }";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const GRAB_TIMEOUT: Duration = Duration::from_secs(20);

const DEFAULT_INTERVAL: Duration = Duration::from_secs(300);
const DEFAULT_GAP: Duration = Duration::from_secs(5);
/// Mean gray-level difference per pixel. One changed clock digit measures about 3.4 (render test).
pub const DEFAULT_THRESHOLD: f64 = 1.0;
const BAD_READS_TO_ACT: u32 = 2;

/// In source-frame pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendition {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadbackState {
    Ok,
    Frozen,
    Unavailable,
    Off,
}

/// `last_diff` is `null` in JSON when it is infinite, as `JSON.stringify` writes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadbackStatus {
    pub last_check_at: Option<u64>,
    pub last_diff: Option<f64>,
    pub state: ReadbackState,
}

impl ReadbackStatus {
    pub fn off() -> Self {
        Self { last_check_at: None, last_diff: None, state: ReadbackState::Off }
    }
}

/// One gray frame of the stream at `url`, cropped to `region`, or the whole frame at 32x18 without one. Any failure is `None`.
#[async_trait]
pub trait FrameGrab: Send + Sync {
    async fn grab(&self, url: &str, region: Option<Region>) -> Option<Vec<u8>>;
}

/// The overlay clock (`hh:mm:ss`) in a 1920x1080 frame, with a few pixels of slack: overlay.rs draws the
/// progress line at x=24, y=70 (top-left), 22 px, and the clock is its first 8 characters.
const CLOCK_1080: Region = Region { x: 16, y: 62, w: 104, h: 32 };

/// Scales the clock region to a rendition. Rounds half up, as `Math.round` does.
pub fn timer_region(width: u32, height: u32) -> Region {
    let sx = f64::from(width) / 1920.0;
    let sy = f64::from(height) / 1080.0;
    let scale = |v: u32, s: f64| (f64::from(v) * s + 0.5).floor() as u32;
    Region { x: scale(CLOCK_1080.x, sx), y: scale(CLOCK_1080.y, sy), w: scale(CLOCK_1080.w, sx), h: scale(CLOCK_1080.h, sy) }
}

/// The first `RESOLUTION=<w>x<h>` in an attribute line.
fn resolution(attrs: &str) -> Option<(f64, f64)> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    for (at, _) in attrs.match_indices("RESOLUTION=") {
        let rest = &attrs[at + "RESOLUTION=".len()..];
        let w = digits(rest);
        if w == 0 || rest.as_bytes().get(w) != Some(&b'x') {
            continue;
        }
        let after = &rest[w + 1..];
        let h = digits(after);
        if h > 0 {
            return Some((rest[..w].parse().ok()?, after[..h].parse().ok()?));
        }
    }
    None
}

fn pick_rendition(master: &str) -> Option<Rendition> {
    // `/\r?\n/`: a CR is dropped only in front of an LF.
    let parts: Vec<&str> = master.split('\n').collect();
    let last = parts.len() - 1;
    let lines: Vec<&str> = parts.iter().enumerate().map(|(i, l)| if i < last { l.strip_suffix('\r').unwrap_or(l) } else { l }).collect();
    let mut best: Option<(Rendition, bool, f64)> = None;
    for i in 0..lines.len().saturating_sub(1) {
        if !lines[i].starts_with("#EXT-X-STREAM-INF:") {
            continue;
        }
        let url = lines[i + 1].trim();
        if url.is_empty() || url.starts_with('#') || lines[i].contains("VIDEO=\"audio_only\"") {
            continue;
        }
        let (width, height) = resolution(lines[i]).unwrap_or((1920.0, 1080.0));
        let chunked = lines[i].contains("VIDEO=\"chunked\"");
        let area = width * height;
        let better = match &best {
            None => true,
            Some((_, best_chunked, best_area)) => (chunked && !best_chunked) || (chunked == *best_chunked && area > *best_area),
        };
        if better {
            best = Some((Rendition { url: url.to_string(), width: width as u32, height: height as u32 }, chunked, area));
        }
    }
    best.map(|(r, _, _)| r)
}

/// `encodeURIComponent`.
pub(crate) fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One `URLSearchParams` value: a space is `+`, everything but `*-._` and alphanumerics is escaped.
fn form_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' => out.push(b as char),
            _ if b.is_ascii_alphanumeric() => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `new URLSearchParams(pairs).toString()`.
pub(crate) fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{}={}", form_component(k), form_component(v))).collect::<Vec<_>>().join("&")
}

fn string_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty())
}

/// The source rendition of the channel's live HLS, so the overlay text stays legible. Any failure is `None`.
pub async fn live_rendition(http: &dyn Http, channel: &str) -> Option<Rendition> {
    let login = channel.to_lowercase();
    let body = serde_json::json!({ "operationName": "PlaybackAccessToken", "query": TOKEN_QUERY, "variables": { "login": login } });
    let mut token_req = HttpRequest::post_json(GQL_URL, &body);
    token_req.headers.push(("Client-ID".into(), TWITCH_WEB_CLIENT_ID.into()));
    token_req.timeout = Some(REQUEST_TIMEOUT);
    let token_res = http.send(token_req).await.ok()?;
    if !(200..300).contains(&token_res.status) {
        return None;
    }
    let json = token_res.json()?;
    let token = json.get("data")?.get("streamPlaybackAccessToken")?;
    let (value, signature) = (string_field(token, "value")?, string_field(token, "signature")?);
    let mut random = [0u8; 4];
    getrandom::fill(&mut random).ok()?;
    let p = (u32::from_le_bytes(random) % 10_000_000).to_string();
    let qs = form_encode(&[("sig", signature), ("token", value), ("allow_source", "true"), ("allow_audio_only", "false"), ("p", &p)]);
    let mut master_req = HttpRequest::get(&format!("https://usher.ttvnw.net/api/channel/hls/{}.m3u8?{qs}", encode_uri_component(&login)));
    master_req.timeout = Some(REQUEST_TIMEOUT);
    let master = http.send(master_req).await.ok()?;
    if !(200..300).contains(&master.status) {
        return None;
    }
    pick_rendition(&master.text())
}

/// Runs the install's ffmpeg through the `Runner`.
pub struct FfmpegGrab {
    runner: Arc<dyn Runner>,
    ffmpeg: String,
}

impl FfmpegGrab {
    pub fn new(runner: Arc<dyn Runner>, ffmpeg: impl Into<String>) -> Self {
        Self { runner, ffmpeg: ffmpeg.into() }
    }
}

#[async_trait]
impl FrameGrab for FfmpegGrab {
    async fn grab(&self, url: &str, region: Option<Region>) -> Option<Vec<u8>> {
        grab_gray_region(&*self.runner, &self.ffmpeg, url, region).await
    }
}

/// One gray frame, cropped to `region`, or the whole frame at 32x18 without one. Any failure is `None`.
pub async fn grab_gray_region(runner: &dyn Runner, ffmpeg: &str, url: &str, region: Option<Region>) -> Option<Vec<u8>> {
    let (filter, size) = match region {
        Some(r) => (format!("crop={}:{}:{}:{},format=gray", r.w, r.h, r.x, r.y), (r.w * r.h) as usize),
        None => ("scale=32:18,format=gray".to_string(), 32 * 18),
    };
    let args = ["-hide_banner", "-loglevel", "error", "-i", url, "-frames:v", "1", "-vf", &filter, "-f", "rawvideo", "-"];
    let out = runner.run(RunSpec::new(ffmpeg, &args, GRAB_TIMEOUT)).await.ok()?;
    (out.code == Some(0) && out.stdout.len() == size).then_some(out.stdout)
}

fn is_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// A stitched ad (preroll or break) shows a still placeholder; streamlink's twitch plugin tests the same two marks.
pub fn is_ad_playlist(media: &str) -> bool {
    let line_starts = std::iter::once(0).chain(media.char_indices().filter(|(_, c)| is_line_end(*c)).map(|(i, c)| i + c.len_utf8()));
    let starts: Vec<usize> = line_starts.collect();
    let line_end = |from: usize| media[from..].find(is_line_end).map_or(media.len(), |n| from + n);
    for &s in &starts {
        let line = &media[s..line_end(s)];
        if let Some(rest) = line.strip_prefix("#EXT-X-DATERANGE:") {
            if rest.contains("CLASS=\"twitch-stitched-ad\"") {
                return true;
            }
        }
    }
    // `^#EXTINF:[^,]*,(.*)$`: the first comma after the tag may sit on a later line.
    let mut from = 0;
    for &s in &starts {
        if s < from || !media[s..].starts_with("#EXTINF:") {
            continue;
        }
        let after = s + "#EXTINF:".len();
        let Some(comma) = media[after..].find(',') else { return false };
        let title_at = after + comma + 1;
        let end = line_end(title_at);
        let title = media[title_at..end].trim();
        if !title.is_empty() && title != "live" {
            return true;
        }
        from = end;
    }
    false
}

/// Frames of different sizes count as different.
pub fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return f64::INFINITY;
    }
    let sum: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    sum as f64 / a.len() as f64
}

async fn fetch_text(http: &dyn Http, url: &str) -> Option<String> {
    let mut req = HttpRequest::get(url);
    req.timeout = Some(REQUEST_TIMEOUT);
    let res = http.send(req).await.ok()?;
    (200..300).contains(&res.status).then(|| res.text())
}

/// What the read-back asks of its owner and tells it.
#[async_trait]
pub trait ReadbackHooks: Send + Sync {
    /// Helix: only a channel Twitch calls live can be frozen.
    async fn is_channel_live(&self) -> Option<bool>;
    /// The overlay clock stood still twice while Twitch calls the channel live.
    async fn on_frozen(&self);
    fn on_unavailable(&self, why: &str);
    /// Two still whole-frame reads: reported once per episode, never acted on (a still upload looks the same).
    fn on_still(&self);
}

pub struct ReadbackOptions {
    pub channel: String,
    pub grab: Arc<dyn FrameGrab>,
    pub http: Arc<dyn Http>,
    pub client: Arc<dyn SupervisorApi>,
    pub clock: Arc<dyn Clock>,
    pub hooks: Arc<dyn ReadbackHooks>,
    /// `supervisor.overlay`: without it there is no clock, so the whole frame is compared.
    pub overlay: bool,
    pub interval: Option<Duration>,
    pub gap: Option<Duration>,
    pub threshold: Option<f64>,
    /// Test runs only (`STREAMLOOP_TEST_HLS_URL`): read this 1080p HLS instead of asking Twitch for the channel's.
    pub hls_url: Option<String>,
}

struct State {
    busy: bool,
    bad_reads: u32,
    in_outage: bool,
    still_noted: bool,
    stopped: bool,
    current: ReadbackStatus,
}

/// Clears the busy flag however the check ends.
struct Busy<'a>(&'a Mutex<State>);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().busy = false;
    }
}

/// Pulls two frames of the channel's own HLS and checks the overlay clock moved between them.
pub struct TwitchReadback {
    opts: ReadbackOptions,
    state: Mutex<State>,
    timer: Mutex<Option<JoinHandle<()>>>,
}

/// Whether the supervisor is on air with a picture: the `live` answer of the dashboard's stream state.
fn on_air(sup: &Option<SupervisorStatus>) -> bool {
    let Some(sup) = sup else { return false };
    if sup.state.as_deref() != Some("playing") || sup.encoder.as_ref().and_then(|e| e.alive) != Some(true) {
        return false;
    }
    let held = matches!(sup.hold, Some(Some(_)));
    let step = sup.recovery.as_ref().and_then(|r| r.step.as_deref()).unwrap_or("");
    !held && (step.is_empty() || step == "none")
}

impl TwitchReadback {
    pub fn new(opts: ReadbackOptions) -> Arc<Self> {
        Arc::new(Self {
            opts,
            state: Mutex::new(State {
                busy: false,
                bad_reads: 0,
                in_outage: false,
                still_noted: false,
                stopped: false,
                current: ReadbackStatus { last_check_at: None, last_diff: None, state: ReadbackState::Ok },
            }),
            timer: Mutex::new(None),
        })
    }

    pub fn start(self: &Arc<Self>) {
        self.stop();
        self.state.lock().unwrap().stopped = false;
        let weak: Weak<Self> = Arc::downgrade(self);
        let period = self.opts.interval.unwrap_or(DEFAULT_INTERVAL);
        let task = tokio::spawn(async move {
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                let Some(this) = weak.upgrade() else { return };
                this.check().await;
            }
        });
        *self.timer.lock().unwrap() = Some(task);
    }

    pub fn stop(&self) {
        if let Some(task) = self.timer.lock().unwrap().take() {
            task.abort();
        }
        // A check in flight finishes without acting.
        self.state.lock().unwrap().stopped = true;
    }

    pub fn status(&self) -> ReadbackStatus {
        self.state.lock().unwrap().current.clone()
    }

    pub async fn check(&self) {
        {
            let mut st = self.state.lock().unwrap();
            if st.busy {
                return;
            }
            st.busy = true;
        }
        let _busy = Busy(&self.state);
        self.read().await;
    }

    fn now_ms(&self) -> u64 {
        self.opts.clock.now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
    }

    fn reset_bad_reads(&self) {
        self.state.lock().unwrap().bad_reads = 0;
    }

    async fn read(&self) {
        let sup = self.opts.client.status().await;
        // Slate, holds, recoveries and missing keys show no clock, or no stream at all.
        if !on_air(&sup) || sup.as_ref().and_then(|s| s.playing.as_ref()).and_then(|p| p.source.as_deref()) == Some("slate") {
            self.reset_bad_reads();
            return;
        }
        let rendition = match &self.opts.hls_url {
            Some(url) => Some(Rendition { url: url.clone(), width: 1920, height: 1080 }),
            None => live_rendition(&*self.opts.http, &self.opts.channel).await,
        };
        const NO_PLAYLIST: &str = "the channel's video playlist could not be fetched";
        let Some(rendition) = rendition else { return self.unavailable(NO_PLAYLIST) };
        let Some(media) = fetch_text(&*self.opts.http, &rendition.url).await else { return self.unavailable(NO_PLAYLIST) };
        if is_ad_playlist(&media) {
            tracing::debug!("Twitch read-back skipped: an ad is playing");
            self.reset_bad_reads();
            return;
        }

        // Without a duration the overlay shows the title alone, which never moves.
        let duration = sup.as_ref().and_then(|s| s.playing.as_ref()).and_then(|p| p.duration).flatten().unwrap_or(0.0);
        let region = (self.opts.overlay && duration > 0.0).then(|| timer_region(rendition.width, rendition.height));
        let a = self.opts.grab.grab(&rendition.url, region).await;
        if a.is_some() {
            tokio::time::sleep(self.opts.gap.unwrap_or(DEFAULT_GAP)).await;
        }
        let b = match &a {
            Some(_) => self.opts.grab.grab(&rendition.url, region).await,
            None => None,
        };
        if self.state.lock().unwrap().stopped {
            return;
        }
        let (Some(a), Some(b)) = (a, b) else { return self.unavailable("a frame could not be read from Twitch") };

        let diff = mean_abs_diff(&a, &b);
        let bad = diff < self.opts.threshold.unwrap_or(DEFAULT_THRESHOLD);
        let now = self.now_ms();
        let act = {
            let mut st = self.state.lock().unwrap();
            st.in_outage = false;
            st.current = ReadbackStatus { last_check_at: Some(now), last_diff: Some(diff), state: if bad { ReadbackState::Frozen } else { ReadbackState::Ok } };
            if !bad {
                st.bad_reads = 0;
                st.still_noted = false;
                return;
            }
            st.bad_reads += 1;
            st.bad_reads >= BAD_READS_TO_ACT
        };
        tracing::debug!(diff, region = ?region, "Twitch read-back");
        if !act {
            return;
        }
        if region.is_none() {
            let first = !std::mem::replace(&mut self.state.lock().unwrap().still_noted, true);
            if first {
                self.opts.hooks.on_still();
            }
            return;
        }
        if self.opts.hooks.is_channel_live().await == Some(true) && !self.state.lock().unwrap().stopped {
            self.reset_bad_reads();
            self.opts.hooks.on_frozen().await;
        }
    }

    fn unavailable(&self, why: &str) {
        let now = self.now_ms();
        let first = {
            let mut st = self.state.lock().unwrap();
            st.bad_reads = 0;
            st.current.last_check_at = Some(now);
            st.current.state = ReadbackState::Unavailable;
            !std::mem::replace(&mut st.in_outage, true)
        };
        if !first {
            return;
        }
        tracing::warn!(why, "Twitch read-back unavailable");
        self.opts.hooks.on_unavailable(why);
    }
}

impl Drop for TwitchReadback {
    fn drop(&mut self) {
        if let Some(task) = self.timer.lock().unwrap().take() {
            task.abort();
        }
    }
}
