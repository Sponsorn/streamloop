use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Map, Value};
use streamloop_common::encoder::Encoder::{self, Amf, Libx264, Nvenc, Qsv};
use streamloop_common::paths::Layout;
use streamloop_control::clock::TestClock;
use streamloop_control::events::{launcher_event, AlertGate, DiscordKind};
use streamloop_control::runner::{FakeRunner, RunError, RunErrorKind, RunOutput, RunSpec, Runner};
use streamloop_control::setup_check::*;

fn layout() -> Layout {
    Layout::new(Path::new("X:\\root"))
}

fn ffmpeg() -> PathBuf {
    layout().ffmpeg
}

/// The encoder a probe call tests: the argument after `-c:v`.
fn codec_of(spec: &RunSpec) -> String {
    let at = spec.args.iter().position(|a| a == "-c:v").unwrap();
    spec.args[at + 1].clone()
}

/// A runner where only the listed encoders pass their test encode.
struct Working {
    working: Vec<Encoder>,
    calls: Mutex<Vec<RunSpec>>,
}

impl Working {
    fn new(working: &[Encoder]) -> Self {
        Self { working: working.to_vec(), calls: Mutex::new(Vec::new()) }
    }

    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl Runner for Working {
    async fn run(&self, spec: RunSpec) -> Result<RunOutput, RunError> {
        let ok = self.working.iter().any(|e| e.as_str() == codec_of(&spec));
        self.calls.lock().unwrap().push(spec);
        Ok(RunOutput { code: Some(if ok { 0 } else { 1 }), ..RunOutput::default() })
    }
}

fn exit(code: i32) -> Result<RunOutput, RunError> {
    Ok(RunOutput { code: Some(code), ..RunOutput::default() })
}

// ---------------------------------------------------------------- probe_encoder

#[tokio::test]
async fn probes_with_a_two_second_1080p30_test_encode_and_a_20_second_timeout() {
    // Arrange
    let runner = Working::new(&[Nvenc]);
    // Act
    let ok = probe_encoder(&runner, &ffmpeg(), Nvenc).await;
    // Assert
    let call = runner.calls.lock().unwrap()[0].clone();
    assert!(ok);
    assert_eq!(call.program, ffmpeg().to_string_lossy());
    assert_eq!(call.timeout, Duration::from_secs(20));
    let line = call.args.join(" ");
    assert!(line.contains("-f lavfi -i testsrc2=size=1920x1080:rate=30 -t 2 -c:v h264_nvenc -preset p4 -rc cbr"));
    assert!(line.contains("-profile:v high -b:v 4500k -maxrate 4500k -bufsize 9000k -g 60 -pix_fmt yuv420p -f null -"));
}

#[tokio::test]
async fn probe_command_line_is_exact() {
    // Arrange
    let runner = Working::new(&[Nvenc, Amf, Qsv, Libx264]);
    let sets: [(Encoder, &[&str]); 4] = [
        (Nvenc, &["-preset", "p4", "-rc", "cbr", "-bf", "0", "-profile:v", "high"]),
        (Amf, &["-usage", "transcoding", "-quality", "balanced", "-rc", "cbr", "-bf", "0", "-profile:v", "high"]),
        (Qsv, &["-preset", "medium", "-look_ahead", "0", "-bf", "0", "-profile:v", "high"]),
        (Libx264, &["-preset", "veryfast", "-sc_threshold", "0", "-keyint_min", "60"]),
    ];
    for (e, set) in sets {
        // Act
        probe_encoder(&runner, &ffmpeg(), e).await;
        // Assert
        let mut want: Vec<&str> = vec!["-hide_banner", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30", "-t", "2", "-c:v", e.as_str()];
        want.extend(set);
        want.extend(["-b:v", "4500k", "-maxrate", "4500k", "-bufsize", "9000k", "-g", "60", "-pix_fmt", "yuv420p", "-f", "null", "-"]);
        let got = runner.calls.lock().unwrap().last().unwrap().args.clone();
        assert_eq!(got, want, "{}", e.as_str());
    }
}

#[tokio::test]
async fn a_probe_that_cannot_start_or_times_out_is_a_failure() {
    // Arrange
    let runner = FakeRunner::new();
    runner.push(Err(RunError { kind: RunErrorKind::Spawn, message: "no ffmpeg".into() }));
    runner.push(Err(RunError { kind: RunErrorKind::Timeout, message: "slow".into() }));
    runner.push(exit(1));
    // Act / Assert
    for _ in 0..3 {
        assert!(!probe_encoder(&runner, &ffmpeg(), Nvenc).await);
    }
}

// ---------------------------------------------------------------- pick_encoder

#[tokio::test]
async fn falls_back_from_a_failing_nvenc_to_amf() {
    // Act
    let r = pick_encoder(&Working::new(&[Amf, Libx264]), &ffmpeg(), None, Duration::ZERO).await;
    // Assert
    assert_eq!(r, EncoderPick { encoder: Amf, tried: vec![Nvenc, Amf], software: false, works: true });
}

#[tokio::test]
async fn ends_on_libx264_when_no_hardware_encoder_works() {
    // Act
    let r = pick_encoder(&Working::new(&[Libx264]), &ffmpeg(), None, Duration::ZERO).await;
    // Assert
    assert_eq!(r, EncoderPick { encoder: Libx264, tried: vec![Nvenc, Amf, Qsv, Libx264], software: true, works: true });
}

#[tokio::test]
async fn tries_the_preferred_encoder_first() {
    // Arrange
    let runner = Working::new(&[Qsv, Nvenc]);
    // Act
    let r = pick_encoder(&runner, &ffmpeg(), Some(Qsv), Duration::ZERO).await;
    // Assert
    assert_eq!(r, EncoderPick { encoder: Qsv, tried: vec![Qsv], software: false, works: true });
    assert_eq!(runner.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn retries_the_preferred_encoder_once_after_a_pause_and_keeps_it_when_the_second_try_passes() {
    // Arrange
    let runner = Arc::new(FakeRunner::new());
    runner.push(exit(1));
    runner.push(exit(0));
    let started = tokio::time::Instant::now();
    // Act
    let r = pick_encoder(runner.as_ref(), &ffmpeg(), Some(Nvenc), Duration::from_secs(2)).await;
    // Assert
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    assert_eq!(runner.calls().len(), 2);
    assert_eq!(r, EncoderPick { encoder: Nvenc, tried: vec![Nvenc], software: false, works: true });
}

#[tokio::test]
async fn probes_at_most_four_times_and_reports_when_nothing_works() {
    // Arrange
    let runner = Working::new(&[]);
    // Act
    let r = pick_encoder(&runner, &ffmpeg(), Some(Amf), Duration::ZERO).await;
    // Assert
    assert_eq!(runner.count(), 4);
    assert!(!r.works);
    assert_eq!(r.tried, vec![Amf, Nvenc, Qsv, Libx264]);
    assert_eq!(r.encoder, Libx264);
}

// ---------------------------------------------------------------- recommend_cache_mb

#[test]
fn recommends_a_tenth_of_the_free_space_within_500_and_4096_mb() {
    // Act / Assert
    assert_eq!(recommend_cache_mb(100 * 1024, &[]), (4096, None));
    assert_eq!(recommend_cache_mb(10 * 1024, &[]), (1024, None));
    assert_eq!(recommend_cache_mb(3 * 1024, &[]), (500, None));
}

#[test]
fn sets_the_minimum_with_a_warning_below_2_gb_free() {
    // Act
    let r = recommend_cache_mb(1536, &[]);
    // Assert
    assert_eq!(r, (50, Some("Less than 2 GB free on this drive. The download cache is set to its minimum.".into())));
}

#[test]
fn makes_room_for_the_two_longest_videos_so_the_next_one_downloads_while_the_current_one_plays() {
    // Arrange: the box's 8.5 h and 7.5 h videos, at 0.5 MB a second
    let longest = [30_474.0, 26_953.0, 600.0];
    // Act
    let r = recommend_cache_mb(400 * 1024, &longest);
    // Assert: (30474 + 26953) * 0.5 = 28714 MB, well past the old 4096 MB ceiling
    assert_eq!(r, (28_714, None));
}

#[test]
fn keeps_at_least_500_mb_when_the_playlist_has_only_short_videos() {
    // Act / Assert
    assert_eq!(recommend_cache_mb(400 * 1024, &[120.0, 90.0]), (500, None));
}

#[test]
fn caps_at_half_the_free_space_and_says_long_videos_play_from_youtube_while_they_download() {
    // Act: 40 GB free, so at most 20 GB can go to the cache
    let (mb, warning) = recommend_cache_mb(40 * 1024, &[30_474.0, 26_953.0]);
    // Assert
    assert_eq!(mb, 20 * 1024);
    assert_eq!(
        warning.as_deref(),
        Some("The two longest videos need about 28 GB, but only 20 GB can be spared on this drive. Long videos play from YouTube while they download.")
    );
}

#[test]
fn matches_every_cache_fixture() {
    // Arrange
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cache.json");
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(cases.len(), 26);
    for case in cases {
        let input = &case["input"];
        let free = input["freeMb"].as_u64().unwrap();
        let durations: Vec<f64> = input["durationsS"].as_array().map(|a| a.iter().map(|d| d.as_f64().unwrap()).collect()).unwrap_or_default();
        // Act
        let (mb, warning) = recommend_cache_mb(free, &durations);
        // Assert
        let want = &case["output"];
        assert_eq!(Value::from(mb), want["mb"], "{}", case["name"]);
        assert_eq!(warning.map_or(Value::Null, Value::from), want["warning"], "{}", case["name"]);
    }
}

// ---------------------------------------------------------------- check_dependencies

#[test]
fn lists_exactly_the_one_missing_file_as_not_ok() {
    // Arrange
    let l = Layout::new(Path::new("R:\\"));
    let missing = l.token_server.clone();
    // Act
    let deps = check_dependencies(&l, |p| p != missing);
    // Assert
    let bad: Vec<&Dependency> = deps.iter().filter(|d| !d.ok).collect();
    assert_eq!(bad, vec![&Dependency { name: "Token server".into(), ok: false, path: missing.to_string_lossy().into_owned() }]);
    assert!(deps.iter().any(|d| d.path == "C:\\Windows\\Fonts\\malgun.ttf"));
    assert_eq!(deps.len(), 9);
}

#[test]
fn the_supervisor_dependency_is_the_control_exe() {
    // Arrange
    let l = layout();
    // Act
    let deps = check_dependencies(&l, |_| true);
    // Assert
    assert_eq!(deps[0].name, "Supervisor");
    assert_eq!(deps[0].path, l.exe.to_string_lossy());
    let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["Supervisor", "Standby slate", "ffmpeg", "yt-dlp", "Deno", "PO-token plugin", "Token server", "Token server modules", "Overlay font"]
    );
}

// ---------------------------------------------------------------- run_setup_check

fn root_with_data(name: &str) -> (Layout, PathBuf) {
    let dir = std::env::temp_dir().join(format!("sl-setup-check-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    (Layout::new(&dir), dir)
}

#[tokio::test]
async fn writes_the_report_to_data_setup_check_json_with_the_keys_node_wrote() {
    // Arrange
    let (l, dir) = root_with_data("report");
    // Act
    let report = run_setup_check(&l, &Working::new(&[Nvenc]), 10_240, &[]).await;
    // Assert
    assert_eq!(report.encoder, ReportEncoder { chosen: Nvenc, tried: vec![Nvenc], software: false, works: true });
    assert_eq!(report.cache, ReportCache { free_mb: 10_240, recommended_mb: 1024, warning: None });
    let text = std::fs::read_to_string(&l.setup_report).unwrap();
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["encoder"], serde_json::json!({"chosen": "h264_nvenc", "tried": ["h264_nvenc"], "software": false, "works": true}));
    assert_eq!(json["cache"], serde_json::json!({"freeMb": 10_240, "recommendedMb": 1024, "warning": null}));
    assert_eq!(json["dependencies"][0], serde_json::json!({"name": "Supervisor", "ok": false, "path": l.exe.to_string_lossy()}));
    let keys: std::collections::BTreeSet<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["at", "cache", "dependencies", "encoder"].into_iter().collect());
    assert!(json["at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(read_setup_report(&l), Some(report));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn reports_works_false_only_when_every_encoder_failed() {
    // Arrange
    let (l, dir) = root_with_data("none");
    // Act
    let report = run_setup_check(&l, &Working::new(&[]), 1, &[]).await;
    // Assert
    assert_eq!(report.encoder, ReportEncoder { chosen: Libx264, tried: vec![Nvenc, Amf, Qsv, Libx264], software: true, works: false });
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reads_a_report_node_wrote() {
    // Arrange
    let (l, dir) = root_with_data("node-file");
    let node = r#"{
  "at": "2026-10-23T10:00:00.000Z",
  "encoder": { "chosen": "h264_nvenc", "tried": ["h264_nvenc"], "software": false, "works": true },
  "cache": { "freeMb": 90000, "recommendedMb": 4096, "warning": null },
  "dependencies": [{ "name": "Supervisor", "ok": true, "path": "C:\\sl\\supervisor\\streamloop-supervisor.exe" }]
}"#;
    std::fs::write(&l.setup_report, node).unwrap();
    // Act
    let report = read_setup_report(&l).unwrap();
    // Assert
    assert_eq!(report.cache.recommended_mb, 4096);
    assert_eq!(report.dependencies[0].name, "Supervisor");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_missing_or_broken_report_reads_as_none() {
    // Arrange
    let (l, dir) = root_with_data("broken");
    assert_eq!(read_setup_report(&l), None);
    std::fs::write(&l.setup_report, "{").unwrap();
    // Act / Assert
    assert_eq!(read_setup_report(&l), None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn obs_running_answers_without_panicking() {
    // Act: the answer depends on the machine; the call must complete and, on a box without OBS, say false
    let _ = obs_running();
}

// ---------------------------------------------------------------- launch guard

struct Rig {
    guard: LaunchGuard,
    obs: Arc<AtomicBool>,
    clock: Arc<TestClock>,
    events: Arc<Mutex<Vec<(String, Map<String, Value>)>>>,
    saved: Arc<Mutex<Vec<(Encoder, Encoder)>>>,
    gets: Arc<Mutex<(Encoder, Option<Encoder>)>>,
}

fn rig(runner: Arc<dyn Runner>, current: Encoder, preferred: Option<Encoder>) -> Rig {
    let obs = Arc::new(AtomicBool::new(false));
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_000)));
    let events = Arc::new(Mutex::new(Vec::new()));
    let saved = Arc::new(Mutex::new(Vec::new()));
    let gets = Arc::new(Mutex::new((current, preferred)));
    let (o, e, s, g) = (obs.clone(), events.clone(), saved.clone(), gets.clone());
    let guard = LaunchGuard::new(LaunchGuardOptions {
        layout: layout(),
        runner,
        obs_running: Box::new(move || o.load(Ordering::SeqCst)),
        exists: Box::new(|_| true),
        clock: clock.clone(),
        get_encoders: Box::new(move || *g.lock().unwrap()),
        save_encoders: Box::new(move |enc, pref| {
            s.lock().unwrap().push((enc, pref));
            Ok(())
        }),
        on_event: Box::new(move |kind, detail| e.lock().unwrap().push((kind.to_string(), detail))),
    });
    Rig { guard, obs, clock, events, saved, gets }
}

fn kinds(r: &Rig) -> Vec<String> {
    r.events.lock().unwrap().iter().map(|(k, _)| k.clone()).collect()
}

#[tokio::test]
async fn sends_the_obs_block_to_discord_as_critical() {
    // Arrange
    let e = launcher_event("supervisor-blocked-by-obs", Map::new(), UNIX_EPOCH);
    // Act
    let d = AlertGate::default().describe(&e);
    // Assert
    assert_eq!(e.level, "critical");
    assert_eq!(d.message, OBS_RUNNING);
    let alert = d.discord.unwrap();
    assert_eq!((alert.kind, alert.level.as_str()), (DiscordKind::Critical, "error"));
    assert_eq!(alert.vars, vec![("message".to_string(), Value::from(OBS_RUNNING))]);
}

#[tokio::test]
async fn sends_one_critical_per_obs_block_through_the_launch_guard() {
    // Arrange: one long block, then OBS closes, opens, closes, opens again
    let mut r = rig(Arc::new(Working::new(&[Libx264])), Libx264, None);
    let mut gate = AlertGate::default();
    let mut sent = Vec::new();
    let checks = |r: &mut Rig, n: usize, obs: bool| {
        r.obs.store(obs, Ordering::SeqCst);
        n
    };
    // Act
    for _ in 0..checks(&mut r, 5, true) {
        r.guard.check().await;
    }
    let after_one_block = r.events.lock().unwrap().len();
    for (n, obs) in [(2, false), (3, true), (1, false)] {
        for _ in 0..checks(&mut r, n, obs) {
            r.guard.check().await;
        }
    }
    for (kind, detail) in r.events.lock().unwrap().iter() {
        if let Some(d) = gate.describe(&launcher_event(kind, detail.clone(), UNIX_EPOCH)).discord {
            sent.push(d.kind);
        }
    }
    // Assert
    assert_eq!(after_one_block, 1);
    assert_eq!(sent, vec![DiscordKind::Critical, DiscordKind::Critical]);
}

#[test]
fn sends_encoder_fallback_as_critical_with_both_encoders_named() {
    // Arrange
    let mut detail = Map::new();
    detail.insert("from".into(), "h264_nvenc".into());
    detail.insert("to".into(), "h264_amf".into());
    let e = launcher_event("encoder-fallback", detail, UNIX_EPOCH);
    // Act
    let d = AlertGate::default().describe(&e);
    // Assert
    let text = "The h264_nvenc encoder failed its test, so the stream uses h264_amf.";
    assert_eq!(e.level, "critical");
    assert_eq!(d.message, text);
    let alert = d.discord.unwrap();
    assert_eq!((alert.kind, alert.level.as_str()), (DiscordKind::Critical, "error"));
    assert_eq!(alert.vars, vec![("message".to_string(), Value::from(text))]);
}

fn change(kind: &str, from: &str, to: &str) -> streamloop_control::events::SupervisorEvent {
    let mut detail = Map::new();
    detail.insert("from".into(), from.into());
    detail.insert("to".into(), to.into());
    launcher_event(kind, detail, UNIX_EPOCH)
}

#[test]
fn alerts_encoder_fallback_once_per_change() {
    // Arrange
    let mut gate = AlertGate::default();
    let mut fb = |from: &str, to: &str| gate.describe(&change("encoder-fallback", from, to)).discord.map(|d| d.kind);
    // Act
    let got = [fb("h264_nvenc", "h264_amf"), fb("h264_nvenc", "h264_amf"), fb("h264_amf", "libx264")];
    // Assert
    assert_eq!(got, [Some(DiscordKind::Critical), None, Some(DiscordKind::Critical)]);
}

#[test]
fn alerts_the_same_fallback_again_after_the_encoder_was_restored() {
    // Arrange
    let mut gate = AlertGate::default();
    // Act
    let mut step = |k: &str, from: &str, to: &str| gate.describe(&change(k, from, to)).discord.map(|d| d.kind);
    let got = [
        step("encoder-fallback", "h264_nvenc", "h264_amf"),
        step("encoder-restored", "h264_amf", "h264_nvenc"),
        step("encoder-fallback", "h264_nvenc", "h264_amf"),
    ];
    // Assert
    assert_eq!(got, [Some(DiscordKind::Critical), Some(DiscordKind::Recovery), Some(DiscordKind::Critical)]);
}

#[test]
fn sends_encoder_restored_as_an_info_all_clear_through_the_recovery_toggle() {
    // Arrange
    let e = change("encoder-restored", "h264_amf", "h264_nvenc");
    // Act
    let d = AlertGate::default().describe(&e);
    // Assert
    assert_eq!(e.level, "info");
    assert_eq!(d.message, "The h264_nvenc encoder works again, so the stream uses it.");
    let alert = d.discord.unwrap();
    assert_eq!((alert.kind, alert.level.as_str()), (DiscordKind::Recovery, "info"));
    assert_eq!(alert.vars, vec![("step".to_string(), Value::from("Back on the h264_nvenc encoder"))]);
}

#[test]
fn sends_ffmpeg_missing_as_critical() {
    // Act
    let d = AlertGate::default().describe(&launcher_event("ffmpeg-missing", Map::new(), UNIX_EPOCH));
    // Assert
    assert_eq!(d.discord.unwrap().vars, vec![("message".to_string(), Value::from("ffmpeg is missing from the install. Reinstall StreamLoop from the full ZIP."))]);
}

#[test]
fn sends_no_encoder_as_critical() {
    // Act
    let d = AlertGate::default().describe(&launcher_event("no-encoder", Map::new(), UNIX_EPOCH));
    // Assert
    assert_eq!(d.discord.unwrap().vars, vec![("message".to_string(), Value::from("No encoder works on this PC. Check the graphics driver."))]);
}

#[tokio::test]
async fn lets_the_launch_go_ahead_when_the_current_encoder_works() {
    // Arrange
    let mut r = rig(Arc::new(Working::new(&[Nvenc])), Nvenc, None);
    // Act
    let held = r.guard.check().await;
    // Assert
    assert_eq!(held, None);
    assert!(kinds(&r).is_empty());
    assert!(r.saved.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn falls_back_saves_both_encoders_and_comes_back_when_the_preferred_one_works_again() {
    // Arrange: nvenc is preferred and failing, so amf runs
    let mut r = rig(Arc::new(Working::new(&[Amf, Libx264])), Nvenc, None);
    // Act
    let first = r.guard.check().await;
    // Assert
    assert_eq!(first, None);
    assert_eq!(*r.saved.lock().unwrap(), vec![(Amf, Nvenc)]);
    let (kind, detail) = r.events.lock().unwrap()[0].clone();
    assert_eq!(kind, "encoder-fallback");
    assert_eq!((detail["from"].as_str(), detail["to"].as_str()), (Some("h264_nvenc"), Some("h264_amf")));
    // Arrange: the config now names amf with nvenc preferred, and nvenc works again
    *r.gets.lock().unwrap() = (Amf, Some(Nvenc));
    let mut back = rig(Arc::new(Working::new(&[Nvenc])), Amf, Some(Nvenc));
    // Act
    let second = back.guard.check().await;
    // Assert
    assert_eq!(second, None);
    assert_eq!(kinds(&back), vec!["encoder-restored"]);
    assert_eq!(*back.saved.lock().unwrap(), vec![(Nvenc, Nvenc)]);
}

#[tokio::test(start_paused = true)]
async fn holds_the_launch_for_60_seconds_after_no_encoder_works_and_reports_it_once() {
    // Arrange
    let mut r = rig(Arc::new(Working::new(&[])), Libx264, None);
    // Act
    let first = r.guard.check().await;
    let second = r.guard.check().await;
    r.clock.advance(Duration::from_secs(61));
    let third = r.guard.check().await;
    // Assert
    assert_eq!((first.as_deref(), second.as_deref(), third.as_deref()), (Some(NO_ENCODER), Some(NO_ENCODER), Some(NO_ENCODER)));
    assert_eq!(kinds(&r), vec!["no-encoder"]);
}

#[tokio::test]
async fn holds_the_launch_when_ffmpeg_is_missing_and_reports_it_once() {
    // Arrange
    let r = rig(Arc::new(Working::new(&[Libx264])), Libx264, None);
    let events = r.events.clone();
    let mut guard = LaunchGuard::new(LaunchGuardOptions {
        layout: layout(),
        runner: Arc::new(Working::new(&[Libx264])),
        obs_running: Box::new(|| false),
        exists: Box::new(|_| false),
        clock: r.clock.clone(),
        get_encoders: Box::new(|| (Libx264, None)),
        save_encoders: Box::new(|_, _| Ok(())),
        on_event: Box::new(move |kind, detail| events.lock().unwrap().push((kind.to_string(), detail))),
    });
    // Act
    let a = guard.check().await;
    let b = guard.check().await;
    // Assert
    assert_eq!((a.as_deref(), b.as_deref()), (Some(FFMPEG_MISSING), Some(FFMPEG_MISSING)));
    assert_eq!(kinds(&r), vec!["ffmpeg-missing"]);
}
