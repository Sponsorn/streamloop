//! The first-run setup check (encoder probe and pick, cache sizing, dependency list, the report in
//! `data\setup-check.json`) and the guard that runs before each supervisor launch.
//! `data\setup-check.json` keeps 3.0's keys and shapes, so either side reads the other's file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use streamloop_common::encoder::{video_set, Encoder, PROBE_TAIL};
use streamloop_common::paths::Layout;
use streamloop_common::time::iso_now;

use crate::clock::Clock;
use crate::runner::{RunSpec, Runner};

pub const OBS_RUNNING: &str = "OBS is running, and 2.x may be streaming on the same key. Close OBS to start 3.0.";
pub const NO_ENCODER: &str = "No encoder works on this PC. Check the graphics driver.";
pub const FFMPEG_MISSING: &str = "ffmpeg is missing from the install. Reinstall StreamLoop from the full ZIP.";

const ORDER: [Encoder; 4] = [Encoder::Nvenc, Encoder::Amf, Encoder::Qsv, Encoder::Libx264];
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Pause before a failed preferred encoder gets its second probe, so a blip does not downgrade it.
const BLIP_RETRY: Duration = Duration::from_secs(2);
const NO_ENCODER_RECHECK: Duration = Duration::from_secs(60);
const FONT: &str = "C:\\Windows\\Fonts\\malgun.ttf";
/// A 1080p YouTube download, with headroom: the box measured 0.4 MB a second (12,197 MB for 8.5 h).
const CACHE_MB_PER_SECOND: f64 = 0.5;

/// `works` is false only when every encoder failed; `encoder` is then the last one tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderPick {
    pub encoder: Encoder,
    pub tried: Vec<Encoder>,
    pub software: bool,
    pub works: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub ok: bool,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEncoder {
    pub chosen: Encoder,
    pub tried: Vec<Encoder>,
    pub software: bool,
    pub works: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCache {
    #[serde(rename = "freeMb")]
    pub free_mb: u64,
    #[serde(rename = "recommendedMb")]
    pub recommended_mb: u64,
    pub warning: Option<String>,
}

/// The file at `data\setup-check.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupReport {
    pub at: String,
    pub encoder: ReportEncoder,
    pub cache: ReportCache,
    pub dependencies: Vec<Dependency>,
}

/// The exact test encode: 2 s of 1080p30 through the supervisor's own arguments for `e`.
fn probe_args(e: Encoder) -> Vec<String> {
    let head = ["-hide_banner", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30", "-t", "2", "-c:v", e.as_str()];
    let tail = ["-f", "null", "-"];
    head.iter().chain(video_set(e)).chain(PROBE_TAIL).chain(tail.iter()).map(|a| a.to_string()).collect()
}

/// True when the test encode exits 0 within 20 s.
pub async fn probe_encoder(runner: &dyn Runner, ffmpeg: &Path, e: Encoder) -> bool {
    let spec = RunSpec { program: ffmpeg.to_string_lossy().into_owned(), args: probe_args(e), stdin: None, timeout: PROBE_TIMEOUT, env: Vec::new() };
    matches!(runner.run(spec).await, Ok(out) if out.code == Some(0))
}

/// Tries `preferred` first, then the rest in nvenc, amf, qsv, libx264 order. With a non-zero `retry_after`, a failed
/// preferred encoder gets one more probe after that pause.
pub async fn pick_encoder(runner: &dyn Runner, ffmpeg: &Path, preferred: Option<Encoder>, retry_after: Duration) -> EncoderPick {
    let order: Vec<Encoder> = match preferred {
        Some(p) => std::iter::once(p).chain(ORDER.into_iter().filter(|e| *e != p)).collect(),
        None => ORDER.to_vec(),
    };
    let mut tried = Vec::new();
    for enc in order {
        tried.push(enc);
        let mut ok = probe_encoder(runner, ffmpeg, enc).await;
        if !ok && Some(enc) == preferred && !retry_after.is_zero() {
            tokio::time::sleep(retry_after).await;
            ok = probe_encoder(runner, ffmpeg, enc).await;
        }
        if ok {
            return EncoderPick { encoder: enc, tried, software: enc == Encoder::Libx264, works: true };
        }
    }
    let last = *tried.last().expect("the order is never empty");
    EncoderPick { encoder: last, tried, software: last == Encoder::Libx264, works: false }
}

/// With the playlist's durations, room for its two longest videos, so the next one downloads while the current
/// one plays; without them, a share of the free space. Never more than half of it.
pub fn recommend_cache_mb(free_mb: u64, durations_s: &[f64]) -> (u64, Option<String>) {
    if free_mb < 2048 {
        return (50, Some("Less than 2 GB free on this drive. The download cache is set to its minimum.".into()));
    }
    let spare = (free_mb / 2).min(free_mb - 2048);
    let mut longest: Vec<f64> = durations_s.iter().copied().filter(|d| *d > 0.0).collect();
    longest.sort_by(|a, b| b.total_cmp(a));
    longest.truncate(2);
    if longest.is_empty() {
        return ((free_mb / 10).max(500).min(4096).min(spare).max(50), None);
    }
    let need = (((longest[0] + longest.get(1).copied().unwrap_or(0.0)) * CACHE_MB_PER_SECOND).ceil() as u64).max(500);
    if need <= spare {
        return (need, None);
    }
    // JavaScript's Math.round: halves go up.
    let gb = |mb: u64| (mb as f64 / 1024.0 + 0.5).floor() as u64;
    (
        spare,
        Some(format!(
            "The two longest videos need about {} GB, but only {} GB can be spared on this drive. Long videos play from YouTube while they download.",
            gb(need),
            gb(spare)
        )),
    )
}

/// What the supervisor needs at run time. The Supervisor row is the control process's own exe.
pub fn check_dependencies(layout: &Layout, exists: impl Fn(&Path) -> bool) -> Vec<Dependency> {
    let files: [(&str, PathBuf); 9] = [
        ("Supervisor", layout.exe.clone()),
        ("Standby slate", layout.slate.clone()),
        ("ffmpeg", layout.ffmpeg.clone()),
        ("yt-dlp", layout.ytdlp.clone()),
        ("Deno", layout.deno.clone()),
        ("PO-token plugin", layout.plugins.join("bgutil-ytdlp-pot-provider.zip")),
        ("Token server", layout.token_server.clone()),
        ("Token server modules", layout.root.join("pot").join("node_modules")),
        ("Overlay font", PathBuf::from(FONT)),
    ];
    files.into_iter().map(|(name, path)| Dependency { name: name.into(), ok: exists(&path), path: path.to_string_lossy().into_owned() }).collect()
}

/// Probes every encoder in order, sizes the cache from `free_mb` and writes `data\setup-check.json`.
pub async fn run_setup_check(layout: &Layout, runner: &dyn Runner, free_mb: u64, durations_s: &[f64]) -> SetupReport {
    let pick = pick_encoder(runner, &layout.ffmpeg, None, Duration::ZERO).await;
    let (recommended_mb, warning) = recommend_cache_mb(free_mb, durations_s);
    let report = SetupReport {
        at: iso_now(),
        encoder: ReportEncoder { chosen: pick.encoder, tried: pick.tried, software: pick.software, works: pick.works },
        cache: ReportCache { free_mb, recommended_mb, warning },
        dependencies: check_dependencies(layout, |p| p.exists()),
    };
    let written = std::fs::create_dir_all(&layout.data)
        .and_then(|()| std::fs::write(&layout.setup_report, serde_json::to_string_pretty(&report).expect("a report serialises")));
    if let Err(err) = written {
        tracing::warn!(%err, "Writing the setup report failed");
    }
    report
}

/// `None` when the file is missing or unreadable.
pub fn read_setup_report(layout: &Layout) -> Option<SetupReport> {
    serde_json::from_str(&std::fs::read_to_string(&layout.setup_report).ok()?).ok()
}

/// Whether `obs64.exe` is running, from a process snapshot. Fails open: any error reads as not running.
#[cfg(windows)]
pub fn obs_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    // SAFETY: the snapshot handle is checked and closed on every path, and the entry is zeroed with its size set as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        while more {
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("obs64.exe") {
                found = true;
                break;
            }
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(not(windows))]
pub fn obs_running() -> bool {
    false
}

/// The configured `encoder` and `preferredEncoder`.
pub type GetEncoders = Box<dyn Fn() -> (Encoder, Option<Encoder>) + Send + Sync>;
/// Always writes both, so the first fallback also records what to come back to.
pub type SaveEncoders = Box<dyn Fn(Encoder, Encoder) -> Result<(), String> + Send + Sync>;
pub type OnEvent = Box<dyn Fn(&str, Map<String, Value>) + Send + Sync>;

pub struct LaunchGuardOptions {
    pub layout: Layout,
    pub runner: Arc<dyn Runner>,
    pub obs_running: Box<dyn Fn() -> bool + Send + Sync>,
    pub exists: Box<dyn Fn(&Path) -> bool + Send + Sync>,
    pub clock: Arc<dyn Clock>,
    pub get_encoders: GetEncoders,
    pub save_encoders: SaveEncoders,
    pub on_event: OnEvent,
}

/// The launcher's check before each spawn.
pub struct LaunchGuard {
    opts: LaunchGuardOptions,
    obs_reported: bool,
    missing_reported: bool,
    no_encoder_at: Option<SystemTime>,
}

impl LaunchGuard {
    pub fn new(opts: LaunchGuardOptions) -> Self {
        Self { opts, obs_reported: false, missing_reported: false, no_encoder_at: None }
    }

    /// `None` lets the launch go ahead; a reason holds it. Each block reports its event once per streak.
    pub async fn check(&mut self) -> Option<String> {
        if (self.opts.obs_running)() {
            if !self.obs_reported {
                (self.opts.on_event)("supervisor-blocked-by-obs", Map::new());
            }
            self.obs_reported = true;
            return Some(OBS_RUNNING.into());
        }
        self.obs_reported = false;
        let ffmpeg = self.opts.layout.ffmpeg.clone();
        if !(self.opts.exists)(&ffmpeg) {
            if !self.missing_reported {
                (self.opts.on_event)("ffmpeg-missing", Map::new());
            }
            self.missing_reported = true;
            return Some(FFMPEG_MISSING.into());
        }
        self.missing_reported = false;
        if let Some(at) = self.no_encoder_at {
            if self.opts.clock.now().duration_since(at).unwrap_or(Duration::ZERO) < NO_ENCODER_RECHECK {
                return Some(NO_ENCODER.into());
            }
        }

        let (current, preferred) = (self.opts.get_encoders)();
        let preferred = preferred.unwrap_or(current);
        let pick = pick_encoder(self.opts.runner.as_ref(), &ffmpeg, Some(preferred), BLIP_RETRY).await;
        if !pick.works {
            if self.no_encoder_at.is_none() {
                (self.opts.on_event)("no-encoder", Map::new());
            }
            self.no_encoder_at = Some(self.opts.clock.now());
            return Some(NO_ENCODER.into());
        }
        self.no_encoder_at = None;
        if pick.encoder != current {
            if let Err(err) = (self.opts.save_encoders)(pick.encoder, preferred) {
                tracing::warn!(%err, "Saving the encoder failed");
            }
            let mut detail = Map::new();
            detail.insert("from".into(), current.as_str().into());
            detail.insert("to".into(), pick.encoder.as_str().into());
            let kind = if pick.encoder == preferred { "encoder-restored" } else { "encoder-fallback" };
            (self.opts.on_event)(kind, detail);
        }
        None
    }
}
