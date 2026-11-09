use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use streamloop_common::api::{EncoderStatus, Hold, Playing, Recovery, SupervisorStatus};
use streamloop_control::clock::TestClock;
use streamloop_control::http::{FakeHttp, HttpError, HttpResponse};
use streamloop_control::readback::{
    grab_gray_region, is_ad_playlist, live_rendition, mean_abs_diff, timer_region, FfmpegGrab, FrameGrab, ReadbackHooks, ReadbackOptions,
    ReadbackState, Region, Rendition, TwitchReadback, DEFAULT_THRESHOLD,
};
use streamloop_control::runner::{FakeRunner, ProcessRunner, RunError, RunErrorKind, RunOutput};
use streamloop_control::supervisor_client::fake::FakeSupervisor;

const MASTER: &str = concat!(
    "#EXTM3U\n",
    "#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID=\"720p30\",NAME=\"720p\",AUTOSELECT=YES,DEFAULT=YES\n",
    "#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS=\"avc1.4D401F,mp4a.40.2\",VIDEO=\"720p30\",FRAME-RATE=30.000\n",
    "https://video.example/720p30/index.m3u8\n",
    "#EXT-X-MEDIA:TYPE=VIDEO,GROUP-ID=\"chunked\",NAME=\"1080p30 (source)\",AUTOSELECT=YES,DEFAULT=YES\n",
    "#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080,CODECS=\"avc1.64002A,mp4a.40.2\",VIDEO=\"chunked\",FRAME-RATE=30.000\n",
    "https://video.example/chunked/index.m3u8\n",
    "#EXT-X-STREAM-INF:BANDWIDTH=160000,CODECS=\"mp4a.40.2\",VIDEO=\"audio_only\"\n",
    "https://video.example/audio_only/index.m3u8",
);

const LIVE_MEDIA: &str = concat!(
    "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PROGRAM-DATE-TIME:2026-10-25T10:00:00.000Z\n",
    "#EXTINF:2.000,live\nhttps://video.example/seg1.ts\n#EXTINF:2.000,live\nhttps://video.example/seg2.ts",
);

/// Twitch's stitched ad break, in the form streamlink's twitch plugin checks for.
const AD_DATERANGE_MEDIA: &str = concat!(
    "#EXTM3U\n",
    "#EXT-X-DATERANGE:ID=\"stitched-ad-1\",CLASS=\"twitch-stitched-ad\",START-DATE=\"2026-10-25T10:00:00.000Z\",DURATION=30.000\n",
    "#EXTINF:2.000,live\nhttps://video.example/seg1.ts",
);

fn ad_title_media() -> String {
    LIVE_MEDIA.replace("#EXTINF:2.000,live\nhttps://video.example/seg2.ts", "#EXTINF:2.000,Amazon\nhttps://video.example/ad.ts")
}

type Media = Arc<Mutex<Option<String>>>;

/// Twitch's token query, the usher master and the video host, as the read-back reaches them.
fn twitch_http(master: Option<&str>, media: &Media) -> Arc<FakeHttp> {
    let http = Arc::new(FakeHttp::new());
    http.respond("https://gql.twitch.tv/", 200, r#"{"data":{"streamPlaybackAccessToken":{"value":"{\"t\":1}","signature":"sig"}}}"#);
    match master {
        Some(m) => http.respond("https://usher.ttvnw.net/", 200, m),
        None => http.respond("https://usher.ttvnw.net/", 404, "not found"),
    }
    let media = media.clone();
    http.route("https://video.example/", move |_| match media.lock().unwrap().clone() {
        Some(body) => Ok(HttpResponse::new(200, body)),
        None => Ok(HttpResponse::new(404, "not found")),
    });
    http
}

fn media_of(body: &str) -> Media {
    Arc::new(Mutex::new(Some(body.to_string())))
}

/// Hands out queued frames; an empty queue or a `None` entry is a failed grab.
#[derive(Default)]
struct FakeGrab {
    frames: Mutex<VecDeque<Option<Vec<u8>>>>,
    calls: Mutex<Vec<(String, Option<Region>)>>,
    on_grab: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl FakeGrab {
    fn calls(&self) -> Vec<(String, Option<Region>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl FrameGrab for FakeGrab {
    async fn grab(&self, url: &str, region: Option<Region>) -> Option<Vec<u8>> {
        self.calls.lock().unwrap().push((url.to_string(), region));
        if let Some(f) = self.on_grab.lock().unwrap().as_ref() {
            f();
        }
        self.frames.lock().unwrap().pop_front().flatten()
    }
}

#[derive(Default)]
struct Recorder {
    live: Mutex<Option<bool>>,
    frozen: AtomicUsize,
    still: AtomicUsize,
    unavailable: Mutex<Vec<String>>,
}

#[async_trait]
impl ReadbackHooks for Recorder {
    async fn is_channel_live(&self) -> Option<bool> {
        *self.live.lock().unwrap()
    }

    async fn on_frozen(&self) {
        self.frozen.fetch_add(1, Ordering::SeqCst);
    }

    fn on_unavailable(&self, why: &str) {
        self.unavailable.lock().unwrap().push(why.to_string());
    }

    fn on_still(&self) {
        self.still.fetch_add(1, Ordering::SeqCst);
    }
}

impl Recorder {
    fn frozen(&self) -> usize {
        self.frozen.load(Ordering::SeqCst)
    }

    fn still(&self) -> usize {
        self.still.load(Ordering::SeqCst)
    }

    fn unavailable(&self) -> usize {
        self.unavailable.lock().unwrap().len()
    }
}

fn live_status() -> SupervisorStatus {
    SupervisorStatus {
        state: Some("playing".into()),
        encoder: Some(EncoderStatus { alive: Some(true), ..EncoderStatus::default() }),
        recovery: Some(Recovery { step: Some("none".into()), retries: None }),
        hold: Some(None),
        playing: Some(Playing { index: Some(2), source: Some("file".into()), duration: Some(Some(600.0)), ..Playing::default() }),
        ..SupervisorStatus::default()
    }
}

struct Setup {
    rb: Arc<TwitchReadback>,
    grab: Arc<FakeGrab>,
    hooks: Arc<Recorder>,
    http: Arc<FakeHttp>,
    media: Media,
}

struct Opts {
    status: Option<SupervisorStatus>,
    frames: Vec<Option<Vec<u8>>>,
    master: Option<String>,
    media: Option<String>,
    live: Option<bool>,
    overlay: bool,
    hls_url: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self { status: Some(live_status()), frames: Vec::new(), master: Some(MASTER.to_string()), media: Some(LIVE_MEDIA.to_string()), live: Some(true), overlay: true, hls_url: None }
    }
}

fn setup(o: Opts) -> Setup {
    let supervisor = Arc::new(FakeSupervisor::new());
    supervisor.set_status(o.status);
    let grab = Arc::new(FakeGrab { frames: Mutex::new(o.frames.into()), ..FakeGrab::default() });
    let hooks = Arc::new(Recorder { live: Mutex::new(o.live), ..Recorder::default() });
    let media = Arc::new(Mutex::new(o.media));
    let http = twitch_http(o.master.as_deref(), &media);
    let rb = TwitchReadback::new(ReadbackOptions {
        channel: "testchannel".into(),
        grab: grab.clone(),
        http: http.clone(),
        client: supervisor,
        clock: Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_792_929_600))),
        hooks: hooks.clone(),
        overlay: o.overlay,
        interval: None,
        gap: Some(Duration::ZERO),
        threshold: None,
        hls_url: o.hls_url,
    });
    Setup { rb, grab, hooks, http, media }
}

fn frame(fill: u8, n: usize) -> Option<Vec<u8>> {
    Some(vec![fill; n])
}

fn clock_pixels() -> usize {
    let r = timer_region(1920, 1080);
    (r.w * r.h) as usize
}

fn still() -> Option<Vec<u8>> {
    frame(40, clock_pixels())
}

fn moved() -> Option<Vec<u8>> {
    frame(90, clock_pixels())
}

fn whole(fill: u8) -> Option<Vec<u8>> {
    frame(fill, 32 * 18)
}


#[test]
fn mean_abs_diff_is_the_average_absolute_difference_per_pixel() {
    // Arrange
    let a = [0u8, 10, 200, 255];
    let b = [4u8, 10, 190, 255];
    // Act + Assert
    assert_eq!(mean_abs_diff(&a, &b), 3.5);
    assert_eq!(mean_abs_diff(&a, &a), 0.0);
}

#[test]
fn mean_abs_diff_treats_frames_of_different_sizes_as_different() {
    // Act
    let diff = mean_abs_diff(&[0; 4], &[0; 6]);
    // Assert
    assert_eq!(diff, f64::INFINITY);
}


#[test]
fn timer_region_covers_the_overlay_clock_in_a_1920x1080_rendition() {
    assert_eq!(timer_region(1920, 1080), Region { x: 16, y: 62, w: 104, h: 32 });
}

#[test]
fn timer_region_scales_to_a_1280x720_rendition() {
    assert_eq!(timer_region(1280, 720), Region { x: 11, y: 41, w: 69, h: 21 });
}


#[tokio::test]
async fn live_rendition_picks_the_source_rendition_from_the_master_playlist() {
    // Arrange
    let http = twitch_http(Some(MASTER), &media_of(LIVE_MEDIA));
    // Act
    let r = live_rendition(&*http, "TestChannel").await;
    // Assert
    assert_eq!(r, Some(Rendition { url: "https://video.example/chunked/index.m3u8".into(), width: 1920, height: 1080 }));
    assert!(http.requests()[1].url.contains("/api/channel/hls/testchannel.m3u8?"));
}

#[tokio::test]
async fn live_rendition_falls_back_to_the_highest_resolution_when_no_rendition_is_marked_chunked() {
    // Arrange
    let http = twitch_http(Some(&MASTER.replace("chunked", "1080p30")), &media_of(LIVE_MEDIA));
    // Act
    let r = live_rendition(&*http, "c").await;
    // Assert
    assert_eq!(r.unwrap().url, "https://video.example/1080p30/index.m3u8");
}

#[tokio::test]
async fn live_rendition_assumes_1920x1080_when_the_playlist_gives_no_resolution() {
    // Arrange
    let master = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,VIDEO=\"chunked\"\nhttps://video.example/s.m3u8\n";
    let http = twitch_http(Some(master), &media_of(LIVE_MEDIA));
    // Act
    let r = live_rendition(&*http, "c").await;
    // Assert
    assert_eq!(r, Some(Rendition { url: "https://video.example/s.m3u8".into(), width: 1920, height: 1080 }));
}

#[tokio::test]
async fn live_rendition_is_none_when_the_channel_is_offline() {
    // Arrange
    let http = twitch_http(None, &media_of(LIVE_MEDIA));
    // Act + Assert
    assert_eq!(live_rendition(&*http, "c").await, None);
}

#[tokio::test]
async fn live_rendition_is_none_when_the_request_fails() {
    // Arrange
    let http = FakeHttp::new();
    http.route("https://", |_| Err(HttpError::connect("ECONNREFUSED", "network down")));
    // Act + Assert
    assert_eq!(live_rendition(&http, "c").await, None);
}


#[tokio::test]
async fn grab_gray_region_crops_one_gray_frame_with_a_timeout() {
    // Arrange
    let runner = FakeRunner::new();
    runner.push(Ok(RunOutput { code: Some(0), stdout: vec![7; 8], stderr: Vec::new() }));
    // Act
    let out = grab_gray_region(&runner, "ffmpeg.exe", "https://x/s.m3u8", Some(Region { x: 1, y: 2, w: 4, h: 2 })).await;
    // Assert
    assert_eq!(out.map(|o| o.len()), Some(8));
    let call = &runner.calls()[0];
    assert_eq!(call.program, "ffmpeg.exe");
    assert_eq!(call.timeout, Duration::from_secs(20));
    for want in ["-frames:v", "1", "-vf", "crop=4:2:1:2,format=gray", "-f", "rawvideo"] {
        assert!(call.args.iter().any(|a| a == want), "{want} missing from {:?}", call.args);
    }
}

#[tokio::test]
async fn grab_gray_region_scales_the_whole_frame_to_32x18_without_a_region() {
    // Arrange
    let runner = FakeRunner::new();
    runner.push(Ok(RunOutput { code: Some(0), stdout: vec![0; 576], stderr: Vec::new() }));
    // Act
    let out = grab_gray_region(&runner, "f", "u", None).await;
    // Assert
    assert_eq!(out.map(|o| o.len()), Some(576));
    assert!(runner.calls()[0].args.contains(&"scale=32:18,format=gray".to_string()));
}

#[tokio::test]
async fn grab_gray_region_is_none_when_ffmpeg_fails_or_returns_a_short_frame() {
    // Arrange
    let failing = FakeRunner::new();
    failing.push(Err(RunError { kind: RunErrorKind::Wait, message: "exit 1".into() }));
    let short = FakeRunner::new();
    short.push(Ok(RunOutput { code: Some(0), stdout: vec![0; 10], stderr: Vec::new() }));
    let exited = FakeRunner::new();
    exited.push(Ok(RunOutput { code: Some(1), stdout: vec![0; 576], stderr: Vec::new() }));
    // Act + Assert
    assert_eq!(grab_gray_region(&failing, "f", "u", None).await, None);
    assert_eq!(grab_gray_region(&short, "f", "u", None).await, None);
    assert_eq!(grab_gray_region(&exited, "f", "u", None).await, None);
}


#[tokio::test]
async fn two_bad_reads_in_a_row_with_twitch_live_call_on_frozen_once() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), still(), still(), still()], ..Opts::default() });
    // Act
    s.rb.check().await;
    s.rb.check().await;
    // Assert
    assert_eq!(s.hooks.frozen(), 1);
    assert_eq!(s.rb.status().state, ReadbackState::Frozen);
    assert_eq!(s.rb.status().last_diff, Some(0.0));
}

#[tokio::test]
async fn a_test_hls_url_is_read_as_a_1080p_rendition_without_asking_twitch() {
    // Arrange: no usher answer, so only the override can supply a rendition
    let s = setup(Opts { frames: vec![still(), still(), still(), still()], master: None, hls_url: Some("https://video.example/test.m3u8".into()), ..Opts::default() });
    // Act
    s.rb.check().await;
    s.rb.check().await;
    // Assert
    assert_eq!(s.hooks.frozen(), 1);
    assert_eq!(s.grab.calls()[0], ("https://video.example/test.m3u8".to_string(), Some(Region { x: 16, y: 62, w: 104, h: 32 })));
    assert!(s.http.requests().iter().all(|r| r.url.starts_with("https://video.example/")));
}

#[tokio::test]
async fn one_bad_read_then_a_good_one_does_not_act() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), still(), still(), moved(), still(), still()], ..Opts::default() });
    // Act
    s.rb.check().await;
    s.rb.check().await;
    s.rb.check().await;
    // Assert
    assert_eq!(s.hooks.frozen(), 0);
}

#[tokio::test]
async fn two_bad_reads_while_helix_says_the_channel_is_offline_do_not_act() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), still(), still(), still()], live: Some(false), ..Opts::default() });
    // Act
    s.rb.check().await;
    s.rb.check().await;
    // Assert
    assert_eq!(s.hooks.frozen(), 0);
}

#[tokio::test]
async fn a_good_read_reports_ok_with_its_difference() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), moved()], ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    let status = s.rb.status();
    assert_eq!((status.state, status.last_diff), (ReadbackState::Ok, Some(50.0)));
    assert!(status.last_check_at.is_some());
}

async fn grabs_nothing_on(status: Option<SupervisorStatus>) {
    // Arrange
    let s = setup(Opts { status, frames: vec![still(), still()], ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    assert!(s.grab.calls().is_empty());
}

fn with(f: impl FnOnce(&mut SupervisorStatus)) -> Option<SupervisorStatus> {
    let mut s = live_status();
    f(&mut s);
    Some(s)
}

#[tokio::test]
async fn grabs_nothing_on_the_slate() {
    grabs_nothing_on(with(|s| s.playing = Some(Playing { source: Some("slate".into()), ..Playing::default() }))).await;
}

#[tokio::test]
async fn grabs_nothing_on_a_hold() {
    grabs_nothing_on(with(|s| s.hold = Some(Some(Hold { reason: Some("all-failed".into()), ..Hold::default() })))).await;
}

#[tokio::test]
async fn grabs_nothing_on_needs_stream_key() {
    grabs_nothing_on(with(|s| s.state = Some("needs-stream-key".into()))).await;
}

#[tokio::test]
async fn grabs_nothing_on_waiting_for_playlist() {
    grabs_nothing_on(with(|s| s.state = Some("waiting-for-playlist".into()))).await;
}

#[tokio::test]
async fn grabs_nothing_on_a_recovery() {
    grabs_nothing_on(with(|s| s.recovery = Some(Recovery { step: Some("retry".into()), retries: None }))).await;
}

#[tokio::test]
async fn grabs_nothing_without_a_supervisor() {
    grabs_nothing_on(None).await;
}

#[tokio::test]
async fn the_clock_is_cropped_when_the_overlay_is_on_scaled_to_the_rendition() {
    // Arrange: the highest listed rendition is now 1280x720
    let master = MASTER.replace("VIDEO=\"chunked\"", "VIDEO=\"other\"").replace("1920x1080", "640x360");
    let s = setup(Opts { master: Some(master), ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    assert_eq!(s.grab.calls()[0].1, Some(Region { x: 11, y: 41, w: 69, h: 21 }));
}

#[tokio::test]
async fn the_whole_frame_is_compared_when_the_overlay_is_off() {
    // Arrange
    let s = setup(Opts { overlay: false, ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    assert_eq!(s.grab.calls()[0].1, None);
}

#[tokio::test]
async fn the_whole_frame_is_compared_when_the_video_has_no_duration_so_no_clock_shows() {
    // Arrange
    let status = with(|s| s.playing = Some(Playing { source: Some("url".into()), duration: Some(None), ..Playing::default() }));
    let s = setup(Opts { status, ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    assert_eq!(s.grab.calls()[0].1, None);
}

#[tokio::test]
async fn a_missing_playlist_calls_on_unavailable_once_per_outage_and_never_on_frozen() {
    // Arrange
    let s = setup(Opts { master: None, ..Opts::default() });
    // Act
    for _ in 0..3 {
        s.rb.check().await;
    }
    // Assert
    assert_eq!(s.hooks.unavailable(), 1);
    assert!(!s.hooks.unavailable.lock().unwrap()[0].is_empty());
    assert_eq!(s.hooks.frozen(), 0);
    assert!(s.grab.calls().is_empty());
    assert_eq!(s.rb.status().state, ReadbackState::Unavailable);
}

#[tokio::test]
async fn a_failed_frame_grab_calls_on_unavailable_once_and_again_after_a_recovery() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), None, still(), None, still(), moved(), still(), None], ..Opts::default() });
    // Act
    for _ in 0..4 {
        s.rb.check().await;
    }
    // Assert
    assert_eq!(s.hooks.unavailable(), 2);
    assert_eq!(s.hooks.frozen(), 0);
}

#[tokio::test]
async fn whole_frame_reads_only_report_and_note_the_still_episode_once() {
    // Arrange: a still music upload with the overlay off
    let s = setup(Opts { overlay: false, frames: (0..8).map(|_| whole(40)).collect(), ..Opts::default() });
    // Act
    for _ in 0..4 {
        s.rb.check().await;
    }
    // Assert
    assert_eq!(s.hooks.frozen(), 0);
    assert_eq!(s.hooks.still(), 1);
    assert_eq!(s.rb.status().state, ReadbackState::Frozen);
}

#[tokio::test]
async fn a_new_whole_frame_episode_is_noted_after_the_picture_moved() {
    // Arrange
    let frames = [1, 1, 1, 1, 1, 9, 1, 1, 1, 1].map(whole).to_vec();
    let s = setup(Opts { overlay: false, frames, ..Opts::default() });
    // Act
    for _ in 0..5 {
        s.rb.check().await;
    }
    // Assert
    assert_eq!(s.hooks.still(), 2);
}

async fn skips_the_check_during_an_ad(ad: String) {
    // Arrange: one bad read, then an ad, then one bad read
    let s = setup(Opts { frames: vec![still(), still(), still(), still()], ..Opts::default() });
    s.rb.check().await;
    *s.media.lock().unwrap() = Some(ad);
    let grabs_before = s.grab.calls().len();
    // Act
    s.rb.check().await;
    // Assert
    assert_eq!(s.grab.calls().len(), grabs_before);
    *s.media.lock().unwrap() = Some(LIVE_MEDIA.to_string());
    s.rb.check().await;
    assert_eq!(s.hooks.frozen(), 0);
}

#[tokio::test]
async fn the_check_is_skipped_during_a_stitched_ad_date_range_and_the_streak_resets() {
    skips_the_check_during_an_ad(AD_DATERANGE_MEDIA.to_string()).await;
}

#[tokio::test]
async fn the_check_is_skipped_during_a_segment_not_titled_live_and_the_streak_resets() {
    skips_the_check_during_an_ad(ad_title_media()).await;
}

#[tokio::test]
async fn frames_are_grabbed_during_plain_live_segments() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), moved()], ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    assert_eq!(s.grab.calls().len(), 2);
}

#[tokio::test]
async fn a_media_playlist_that_cannot_be_fetched_counts_as_unavailable() {
    // Arrange
    let s = setup(Opts { media: None, ..Opts::default() });
    // Act
    s.rb.check().await;
    // Assert
    assert!(s.grab.calls().is_empty());
    assert_eq!(s.hooks.unavailable(), 1);
}

#[tokio::test]
async fn an_unavailable_check_between_two_bad_reads_resets_the_streak() {
    // Arrange
    let s = setup(Opts { frames: vec![still(), still(), still(), None, still(), still()], ..Opts::default() });
    // Act
    for _ in 0..3 {
        s.rb.check().await;
    }
    // Assert
    assert_eq!(s.hooks.frozen(), 0);
}

#[tokio::test]
async fn a_check_still_in_flight_when_stop_runs_does_nothing() {
    // Arrange: one bad read, then a second bad read whose grab finishes after stop()
    let s = setup(Opts { frames: vec![still(), still()], ..Opts::default() });
    s.rb.check().await;
    let rb = s.rb.clone();
    *s.grab.on_grab.lock().unwrap() = Some(Box::new(move || rb.stop()));
    s.grab.frames.lock().unwrap().extend([still(), still()]);
    // Act
    s.rb.check().await;
    // Assert
    assert_eq!(s.hooks.frozen(), 0);
    *s.grab.on_grab.lock().unwrap() = None;
}

#[test]
fn an_untitled_segment_is_live_so_the_read_back_is_never_silently_skipped() {
    // Act + Assert
    assert!(!is_ad_playlist(&LIVE_MEDIA.replace("#EXTINF:2.000,live", "#EXTINF:2.000,")));
}


#[tokio::test]
async fn live_rendition_is_none_when_the_token_request_fails_with_an_http_error() {
    // Arrange
    let http = FakeHttp::new();
    http.respond("https://gql.twitch.tv/", 500, "oops");
    // Act + Assert
    assert_eq!(live_rendition(&http, "c").await, None);
}


fn json_bytes(v: &Value) -> Vec<u8> {
    v.as_array().unwrap().iter().map(|n| n.as_u64().unwrap() as u8).collect()
}

fn region_of(v: &Value) -> Option<Region> {
    if v.is_null() {
        return None;
    }
    let f = |k: &str| v[k].as_u64().unwrap() as u32;
    Some(Region { x: f("x"), y: f("y"), w: f("w"), h: f("h") })
}

#[tokio::test]
async fn matches_every_readback_fixture() {
    // Arrange
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/readback.json");
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(cases.len(), 54);
    for case in cases {
        let (input, want, name) = (&case["input"], &case["output"], case["name"].as_str().unwrap());
        // Act
        let got: Value = match input["fn"].as_str().unwrap() {
            "meanAbsDiff" => {
                let d = mean_abs_diff(&json_bytes(&input["a"]), &json_bytes(&input["b"]));
                // Whole numbers are integers in the recording.
                if !d.is_finite() { json!("Infinity") } else if d.fract() == 0.0 { json!(d as u64) } else { json!(d) }
            }
            "timerRegion" => {
                let r = timer_region(input["width"].as_u64().unwrap() as u32, input["height"].as_u64().unwrap() as u32);
                json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })
            }
            "liveRendition" => {
                let master = input["master"].as_str();
                let http = twitch_http(master, &media_of(""));
                match live_rendition(&*http, "c").await {
                    Some(r) => json!({ "url": r.url, "width": r.width, "height": r.height }),
                    None => Value::Null,
                }
            }
            "isAdPlaylist" => json!(is_ad_playlist(input["media"].as_str().unwrap())),
            "grabGrayRegion" => {
                let runner = FakeRunner::new();
                match &input["returnedBytes"] {
                    Value::String(_) => runner.push(Err(RunError { kind: RunErrorKind::Wait, message: "exit 1".into() })),
                    n => runner.push(Ok(RunOutput { code: Some(0), stdout: vec![7; n.as_u64().unwrap() as usize], stderr: Vec::new() })),
                }
                let out = grab_gray_region(&runner, "ffmpeg.exe", "https://x/s.m3u8", region_of(&input["region"])).await;
                json!({ "args": runner.calls()[0].args, "length": out.map(|o| o.len()) })
            }
            other => panic!("unknown fixture function {other}"),
        };
        // Assert
        assert_eq!(&got, want, "{name}");
    }
}


const FONT: &str = "C:/Windows/Fonts/malgun.ttf";

fn escape(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:")
}

/// Mirrors overlay.rs's script() for a video with a duration: title, NOT LIVE label, progress line with the clock, and the bar.
fn render(ffmpeg: &str, dir: &Path, start: u32) -> PathBuf {
    let duration = 1200;
    let text = |file: &Path, size: u32, y: u32, expand: bool| {
        format!(
            "drawtext=fontfile='{}':textfile='{}'{}:fontsize={size}:fontcolor=white:x=24:y={y}:box=1:boxcolor=black@0.55:boxborderw=10",
            escape(Path::new(FONT)),
            escape(file),
            if expand { "" } else { ":expansion=none" }
        )
    };
    let (title, label, progress) = (dir.join("title.txt"), dir.join("label.txt"), dir.join(format!("progress-{start}.txt")));
    std::fs::write(&title, "A still video for the read-back test").unwrap();
    std::fs::write(&label, "NOT LIVE").unwrap();
    std::fs::write(&progress, format!("%{{pts:gmtime:{start}:%H\\:%M\\:%S}} / 00:20:00  %{{eif:min(100,100*({start}+t)/{duration}):d}}\\%")).unwrap();
    let graph = [
        "testsrc2=s=1920x1080:r=30,trim=end_frame=1,loop=loop=-1:size=1,setpts=N/30/TB[norm]".to_string(),
        format!(
            "[norm]{},drawtext=fontfile='{}':textfile='{}':expansion=none:fontsize=22:fontcolor=white:x=main_w-text_w-24:y=24:box=1:boxcolor=red@0.8:boxborderw=10,{},drawbox=x=0:y=1074:w=1920:h=6:color=white@0.25:t=fill[base]",
            text(&title, 28, 24, false),
            escape(Path::new(FONT)),
            escape(&label),
            text(&progress, 22, 70, true)
        ),
        "color=c=red:s=1920x6[bar]".to_string(),
        format!("[base][bar]overlay=x='max(-1920,min(0,1920*(({start}+t)/{duration}-1)))':y=1074:shortest=1[outv]"),
    ]
    .join(";");
    let out = dir.join(format!("frame-{start}.png"));
    let status = streamloop_common::procs::command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-filter_complex", &graph, "-map", "[outv]", "-frames:v", "1"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not render the test frame");
    out
}

/// Set STREAMLOOP_FFMPEG to an ffmpeg.exe and run with `--ignored`.
#[tokio::test]
#[ignore = "needs STREAMLOOP_FFMPEG to point at an ffmpeg"]
async fn a_still_picture_with_a_moving_clock_is_a_good_read_and_an_identical_frame_is_not() {
    // Arrange
    let ffmpeg = std::env::var("STREAMLOOP_FFMPEG").expect("STREAMLOOP_FFMPEG names an ffmpeg");
    assert!(Path::new(&ffmpeg).exists(), "STREAMLOOP_FFMPEG does not exist: {ffmpeg}");
    assert!(Path::new(FONT).exists(), "{FONT} is needed for the render");
    let dir = tempfile::tempdir().unwrap();
    let at10 = render(&ffmpeg, dir.path(), 10);
    let at15 = render(&ffmpeg, dir.path(), 15);
    let region = timer_region(1920, 1080);
    let grabber = FfmpegGrab::new(Arc::new(ProcessRunner), ffmpeg);

    // Act
    let a = grabber.grab(&at10.to_string_lossy(), Some(region)).await.unwrap();
    let b = grabber.grab(&at15.to_string_lossy(), Some(region)).await.unwrap();
    let whole1 = grabber.grab(&at10.to_string_lossy(), None).await.unwrap();
    let whole2 = grabber.grab(&at10.to_string_lossy(), None).await.unwrap();

    // Assert
    let clock_diff = mean_abs_diff(&a, &b);
    let still_diff = mean_abs_diff(&whole1, &whole2);
    println!("clock region diff 10 s -> 15 s: {clock_diff:.3}, identical whole frame: {still_diff:.3}, threshold {DEFAULT_THRESHOLD}");
    assert!(clock_diff > 2.0 * DEFAULT_THRESHOLD);
    assert!(still_diff < DEFAULT_THRESHOLD);
}
