//! Spike 5 supervisor (Notion: "Spike 5: supervisor (Rust)"). The control loop owns all state;
//! the API, downloader, resolves, pumps and encoder progress reach it over one channel.

mod api;
mod config;
mod launch;
mod logs;
mod media;
mod overlay;
mod paths;
mod procs;
mod rules;
mod secret;
mod watchdog;
mod ytdlp;

use config::Settings;
use logs::Logs;
use media::{Input, Live, Queue, Shared};
use paths::Paths;
use rules::{secs, ticks, Ladder, Seam, Source, BASE, FRAME};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use watchdog::{Detect, Heal, Verdict};
use ytdlp::{Entry, Resolved, Ytdlp};


fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn watchdog_cfg(s: &Settings) -> media::WatchdogCfg {
    media::WatchdogCfg { freeze_s: s.freeze_seconds, silence_s: s.silence_seconds }
}

fn unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

fn iso_now() -> String {
    iso(unix_ms())
}

/// UTC ISO 8601 with milliseconds, without a date crate.
fn iso(ms: u128) -> String {
    let (secs, ms) = ((ms / 1000) as i64, ms % 1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", rem / 3600, rem / 60 % 60, rem % 60)
}


pub const PORT: u16 = 4417;

/// Test knobs, honoured only with --debug-faults.
#[derive(Default)]
pub struct Dev {
    url_ttl: Option<u64>,
    refresh_margin: Option<u64>,
    limit_rate: Option<String>,
    start: usize,
    max_videos: usize,
    hours: Option<f64>,
}

pub struct Args {
    paths: Paths,
    config: PathBuf,
    /// Dev override for local MediaMTX: replaces ingest and key.
    target: Option<String>,
    debug_faults: bool,
    dev: Dev,
}

const DEV_FLAGS: [&str; 6] = ["url-ttl-s", "url-refresh-margin-s", "limit-rate", "start", "max-videos", "hours"];
const FLAGS: [&str; 5] = ["root", "config", "data", "target", "debug-faults"];

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let opt = |name: &str| argv.iter().position(|a| a == &format!("--{name}")).and_then(|i| argv.get(i + 1)).cloned();
    let num = |name: &str| -> Result<Option<f64>, String> { opt(name).map(|v| v.parse::<f64>().map_err(|_| format!("--{name} {v}: not a number"))).transpose() };
    if let Some(a) = argv.iter().filter_map(|a| a.strip_prefix("--")).find(|a| !FLAGS.contains(a) && !DEV_FLAGS.contains(a)) {
        return Err(format!("unknown flag --{a}"));
    }
    let debug_faults = argv.iter().any(|a| a == "--debug-faults");
    if let Some(d) = DEV_FLAGS.iter().find(|d| opt(d).is_some()).filter(|_| !debug_faults) {
        return Err(format!("--{d} is a test flag and needs --debug-faults"));
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = match opt("root") {
        Some(r) => cwd.join(r),
        // the exe sits in ROOT\supervisor\
        None => std::env::current_exe().ok().and_then(|e| Some(e.parent()?.parent()?.to_path_buf())).ok_or("cannot find the install root; pass --root")?,
    };
    let mut paths = paths::layout(&root);
    if let Some(d) = opt("data") {
        paths.data = cwd.join(d);
    }
    Ok(Args {
        config: opt("config").map_or(paths.config.clone(), |c| cwd.join(c)),
        paths,
        target: opt("target"),
        debug_faults,
        dev: Dev {
            url_ttl: num("url-ttl-s")?.map(|v| v as u64),
            refresh_margin: num("url-refresh-margin-s")?.map(|v| v as u64),
            limit_rate: opt("limit-rate"),
            start: num("start")?.unwrap_or(0.0) as usize,
            max_videos: num("max-videos")?.unwrap_or(0.0) as usize,
            hours: num("hours")?,
        },
    })
}

/// Startup and POST /reload read the config the same way.
fn read_settings(args: &Args) -> Result<Settings, String> {
    let json = std::fs::read_to_string(&args.config).map_err(|e| format!("{}: {e}", args.config.display()))?;
    // --target brings its own key; the protected one is not needed.
    let mut s = config::load(&json, &|b: &str| if args.target.is_some() { Ok(String::new()) } else { secret::unprotect(b) })?;
    if let Some(t) = &args.target {
        let (ingest, key) = t.rsplit_once('/').filter(|(_, k)| !k.is_empty()).ok_or("--target must end in /<key>")?;
        (s.ingest_url, s.stream_key) = (ingest.into(), key.into());
    }
    media::encoder_args("rtmp://x/y", &s.encoder, &watchdog_cfg(&s))?;
    Ok(s)
}

/// `streamloop-supervisor.exe protect`: plain key on stdin, DPAPI blob on stdout.
fn protect_stdin() -> Result<String, String> {
    let mut key = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut key).map_err(|e| format!("cannot read stdin: {e}"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err("no stream key on stdin".into());
    }
    secret::protect(key)
}

#[derive(Debug, PartialEq)]
enum PlaylistAction {
    /// The first list: load it and resume from state.json.
    Load,
    /// Replace the playing list and jump to the video the reload asked for (0 by default).
    Switch,
    /// Superseded by a later reload, or the list already playing.
    Drop,
}

/// `loaded` is the id the playing list came from, `wanted` the id the settings ask for.
fn playlist_action(first: bool, loaded: &str, fetched: &str, wanted: &str) -> PlaylistAction {
    if fetched != wanted {
        PlaylistAction::Drop
    } else if first {
        PlaylistAction::Load
    } else if fetched == loaded {
        PlaylistAction::Drop
    } else {
        PlaylistAction::Switch
    }
}

#[derive(Debug, PartialEq)]
struct KeyReload {
    /// Shown in /status as streamKeyProblem until a reload brings a working key.
    problem: Option<String>,
    /// (level, kind) to log, once per streak of unusable keys.
    event: Option<(&'static str, &'static str)>,
    start_encoder: bool,
}

/// `waiting`: no encoder runs because of the key; `had_problem`: the last key was unusable too.
fn key_after_reload(waiting: bool, had_problem: bool, ready: Result<(), (&'static str, String)>) -> KeyReload {
    match ready {
        Ok(()) => KeyReload { problem: None, event: None, start_encoder: waiting },
        Err((kind, reason)) => KeyReload { problem: Some(reason), event: (!waiting && !had_problem).then_some(("warn", kind)), start_encoder: false },
    }
}

/// The top of /status: state, why (for the states that wait on something), and the key problem.
fn status_head(stopping: bool, waiting_for_key: bool, key_problem: Option<&str>, playlist_problem: Option<&str>, ever_played: bool, recovering: bool) -> Value {
    let (state, reason) = if stopping {
        ("stopping", None)
    } else if waiting_for_key {
        ("needs-stream-key", key_problem)
    } else if playlist_problem.is_some() {
        ("waiting-for-playlist", playlist_problem)
    } else if !ever_played {
        ("starting", None)
    } else if recovering {
        ("recovering", None)
    } else {
        ("playing", None)
    };
    json!({"state": state, "reason": reason, "streamKeyProblem": key_problem})
}

#[derive(Debug, PartialEq)]
struct PlaylistFailure {
    /// Fetch again through the breaker: only the first list, a switch keeps the old one.
    refetch: bool,
    /// Event level, once per failure streak of the first list.
    event: Option<&'static str>,
}

fn playlist_failure(first: bool, streak: bool) -> PlaylistFailure {
    match (first, streak) {
        (true, false) => PlaylistFailure { refetch: true, event: Some("critical") },
        (true, true) => PlaylistFailure { refetch: true, event: None },
        (false, _) => PlaylistFailure { refetch: false, event: Some("warn") },
    }
}

fn encoder_log_line(logs: &Logs, generation: u64, line: &str) {
    logs.process("encoder", generation, &format!("{} [gen {generation}] {line}\n", iso_now()));
}

/// `-progress` keys become EncProgress; every other line goes to encoder.log redacted (at `-v info`
/// that includes the output URL), and a detector line also goes on as Msg::Detect.
fn encoder_stderr_line(logs: &Logs, tx: &Sender<Msg>, generation: u64, line: &str, out_us: &mut Option<String>, speed: &mut String) {
    let t = line.trim();
    match t.split_once('=') {
        Some(("out_time_us", v)) => *out_us = Some(v.to_string()),
        Some(("speed", v)) => *speed = v.trim().to_string(),
        Some(("progress", _)) => {
            if let Some(us) = out_us.as_ref().and_then(|u| u.parse::<i64>().ok()) {
                // The 10 s CPU row (Ctl::sample_cpu) fills the last four columns; these leave them empty.
                logs.csv("encoder", &[unix_ms().to_string(), us.to_string(), speed.clone(), generation.to_string(), String::new(), String::new(), String::new(), String::new()]);
                let _ = tx.send(Msg::EncProgress { generation, media: us as f64 / 1e6, speed: speed.trim_end_matches('x').trim().parse().ok() });
            }
        }
        Some((k, _)) if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {}
        _ if !t.is_empty() => {
            encoder_log_line(logs, generation, t);
            if let Some(d) = watchdog::parse_line(t) {
                let _ = tx.send(Msg::Detect { generation, d });
            }
        }
        _ => {}
    }
}


#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Purpose {
    Play,
    Refresh,
    Size,
    /// A breaker probe (or the bot-check fault): only its outcome matters.
    Probe,
    /// A second URL for the video on air, kept for a fast retry.
    Spare,
}

pub enum Msg {
    /// `id` names the jump's video; when the list has it, it wins over `index`.
    Control { cmd: String, index: Option<i64>, id: Option<String>, reason: Option<String>, reply: Sender<Result<Value, String>> },
    Fault { kind: String, count: Option<u32>, reply: Sender<Result<Value, String>> },
    Shutdown,
    /// `index` and `id`: where a switch to a new playlist starts.
    Reload { index: Option<usize>, id: Option<String>, reply: Sender<Result<Value, String>> },
    /// The playlist id it was fetched for, so a superseded fetch is dropped.
    Playlist(String, Result<Vec<Entry>, String>),
    /// `id` guards against a playlist switch while the call ran: indexes then mean other videos.
    Resolved { index: usize, id: String, purpose: Purpose, result: Result<Resolved, String> },
    FeederExit { seq: u64, code: Option<i32>, frames: u64 },
    EncProgress { generation: u64, media: f64, speed: Option<f64> },
    Detect { generation: u64, d: Detect },
    DlProgress { index: usize, percent: f64 },
    Downloaded { index: usize, id: String, ok: bool, cancelled: bool, bytes: u64, ms: u128, reason: String },
}

enum Job {
    Size(usize, String),
    Download(usize, String, Arc<AtomicBool>),
}


struct Video {
    entry: Entry,
    bytes: Option<u64>,
    on_disk: bool,
    disk_bytes: u64,
    dl_attempts: u32,
    dl_failed: bool,
    skipped: bool,
    retries: u32,
}

/// A second resolved URL for the video on air, so a retry skips yt-dlp and the site's wait.
struct Spare {
    index: usize,
    id: String,
    r: Resolved,
    at: Instant,
}

/// When a URL expires: the dev `--url-ttl-s` counted from the resolve, else the URL's own.
fn expire_of(r: &Resolved, at: Instant, ttl: Option<u64>, now_s: u64) -> Option<u64> {
    ttl.map(|t| now_s - at.elapsed().as_secs() + t).or(r.expire)
}

/// Takes the spare out; returns it only for this exact video and outside the refresh margin.
fn take_spare(spare: &mut Option<Spare>, index: usize, id: &str, now_s: u64, margin_s: u64, ttl: Option<u64>) -> Option<Resolved> {
    let s = spare.take()?;
    let expire = expire_of(&s.r, s.at, ttl, now_s)?;
    (s.id == id && rules::spare_usable(s.index, index, expire, now_s, margin_s)).then_some(s.r)
}

/// A retry's spare, if usable. Using it makes the next spare due at once, so a second
/// failure is fast too (a video gets at most two retries, which bounds this).
#[allow(clippy::too_many_arguments)]
fn retry_spare(slot: &mut Option<Spare>, due: &mut Option<(usize, Instant)>, index: usize, id: &str, now_s: u64, margin_s: u64, ttl: Option<u64>, now: Instant) -> Option<Resolved> {
    let r = take_spare(slot, index, id, now_s, margin_s, ttl)?;
    *due = Some((index, now));
    Some(r)
}

/// Push the next spare resolve for `index` a full `SPARE_AFTER_S` away.
fn delay_spare(due: &mut Option<(usize, Instant)>, index: usize, now: Instant) {
    if let Some((_, at)) = due.as_mut().filter(|(i, _)| *i == index) {
        *at = now + Duration::from_secs_f64(rules::SPARE_AFTER_S);
    }
}

/// Keep an arrived spare. The next one waits a full interval even when this one is already
/// inside the refresh margin, or it would be resolved again at every free slot.
fn store_spare(slot: &mut Option<Spare>, due: &mut Option<(usize, Instant)>, spare: Spare, now: Instant) {
    delay_spare(due, spare.index, now);
    *slot = Some(spare);
}

/// Whether the video `index` on air from its URL needs a spare resolve now.
#[allow(clippy::too_many_arguments)]
fn spare_wanted(due: Option<(usize, Instant)>, index: usize, now: Instant, spare: Option<&Spare>, id: &str, now_s: u64, ttl: Option<u64>, margin_s: u64) -> bool {
    if due.is_none_or(|(i, at)| i != index || now < at) {
        return false;
    }
    // A spare with no known expiry is kept unused rather than resolved again every 5 s.
    spare.is_none_or(|s| s.index != index || s.id != id || expire_of(&s.r, s.at, ttl, now_s).is_some_and(|e| rules::refresh_due(now_s, e, margin_s)))
}

struct Feeder {
    seq: u64,
    child: Arc<Mutex<Child>>,
    index: usize,
    source: Source,
    start_pos: f64,
    offset: i64,
    started: Instant,
    filler: bool,
    expire: Option<u64>,
}

struct FeederMeta {
    name: String,
    started_iso: String,
    offset: i64,
    index: usize,
    source: Source,
    start_pos: f64,
    reason: &'static str,
}

#[derive(Clone)]
struct Target {
    index: usize,
    pos: f64,
    reason: &'static str,
    gated: bool,
}

struct Encoder {
    child: Option<Child>,
    media: f64,
    last_progress: Instant,
    started: Instant,
    restarts: u32,
    failures: u32,
    restart_at: Option<Instant>,
    ingest_was_down: bool,
    /// ffmpeg's own `speed=` (latest), None until it reports one.
    speed: Option<f64>,
    /// The detector durations this encoder runs with; a reload applies at its next start.
    watch: media::WatchdogCfg,
}

/// The output watchdog's state, all in the encoder's media seconds of the current generation.
#[derive(Default)]
struct Watch {
    /// An open freeze / silence: (its start, an `Ignore` for it already reported).
    freeze: Option<(f64, bool)>,
    silence: Option<(f64, bool)>,
    /// Where the last heal cut: nothing before it counts again.
    cut_at: f64,
    /// The last silence retry: (video index, when its feeder went on air).
    retried: Option<(usize, f64)>,
    /// For /status.watchdog: the last verdict, its time and outcome.
    last: Value,
    /// Debug fault on this video's feeders until the watchdog acts.
    fault: Option<(usize, media::FeederFault)>,
}

/// Counters at the last CPU sample, with the pids they belong to.
#[derive(Clone, Copy)]
struct CpuMark {
    enc_pid: Option<u32>,
    feeder_pid: Option<u32>,
    enc: u64,
    feeder: u64,
    idle: u64,
    total: u64,
}

/// CPU as a share of the whole machine (100 = every core busy), sampled every 10 s on the
/// control loop so `status()` only reads these.
#[derive(Default)]
struct Cpu {
    encoder: f64,
    feeder: f64,
    system: f64,
    prev: Option<CpuMark>,
    last: Option<Instant>,
}

pub struct TokenState {
    state: &'static str,
    restarts: u32,
    kill: bool,
    stop: bool,
}

struct Ctl {
    snapshot: api::Snapshot,
    args: Args,
    /// What runs: a reload changes the live fields; ingest, key and encoder wait for a restart.
    settings: Settings,
    /// Why the configured stream key cannot be used; while streaming, the running key carries on.
    key_problem: Option<String>,
    /// No encoder runs until a reload brings a working key.
    waiting_for_key: bool,
    /// Why the first playlist fetch failed; it is fetched again through the breaker.
    playlist_problem: Option<String>,
    /// The playlist id whose in-flight fetch is the breaker's probe.
    playlist_probe: Option<String>,
    /// The playlist id `videos` came from.
    playlist: String,
    playlist_wanted: bool,
    /// Where a pending playlist switch starts, from POST /reload: (index, id).
    switch_to: Option<(usize, Option<String>)>,
    logs: Logs,
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    yt: Arc<Ytdlp>,
    cache: PathBuf,
    videos: Vec<Video>,
    seams: Vec<Seam>,
    enc: Encoder,
    cpu: Cpu,
    watch: Watch,
    live: Option<Feeder>,
    metas: HashMap<u64, FeederMeta>,
    feeder_seq: u64,
    target: Option<Target>,
    resolved: HashMap<usize, (Resolved, Instant)>,
    resolving: HashSet<(usize, bool)>,
    refresh_after: Instant,
    refresh_pending: Option<usize>,
    spare: Option<Spare>,
    /// (index, when) a spare is due for the video on air from its URL.
    spare_due: Option<(usize, Instant)>,
    filler_since: Option<u64>,
    step_offset: i64,
    break_url: Option<(usize, Option<u32>)>,
    jobs: Sender<Job>,
    downloading: Option<(usize, f64, Arc<AtomicBool>)>,
    sizing: bool,
    dl_after: Instant,
    token: Arc<Mutex<TokenState>>,
    step: &'static str,
    consecutive_skips: u32,
    /// Every video skipped: the target waits on the slate for a while before trying again.
    hold: rules::SkipHold,
    /// Videos in a row whose download failed for good.
    dl_fail_streak: u32,
    /// Feeder spawn failures in a row, and when the next spawn may run.
    feeder_fails: u32,
    feeder_after: Instant,
    control_seq: u64,
    ever_played: bool,
    stopping: bool,
    started_at: u128,
    deadline: Option<Instant>,
    cache_bytes: u64,
    max_cache: u64,
    seam_rows: u64,
    counts: HashMap<&'static str, u64>,
    last_state: Instant,
    last_buffer: Instant,
    last_scan: Instant,
    breaker: rules::Breaker,
    t0: Instant,
    last_resolve: Option<Instant>,
    probe_key: Option<(usize, bool)>,
    fake_bot: bool,
    /// Distinct videos answered "unavailable" since the last successful resolve.
    unavailable_streak: HashSet<usize>,
}

const STALL: Duration = Duration::from_secs(10);
/// Videos in a row whose download failed for good before it is a critical event (once per streak).
const DOWNLOAD_FAILS_CRITICAL: u32 = 3;
const FILLER_BELOW_S: f64 = 2.0;

impl Ctl {
    #[allow(clippy::too_many_arguments)]
    fn new(args: Args, settings: Settings, logs: Logs, tx: Sender<Msg>, shared: Arc<Shared>, yt: Arc<Ytdlp>, cache: PathBuf, jobs_tx: Sender<Job>, token: Arc<Mutex<TokenState>>, snapshot: api::Snapshot) -> Ctl {
        let now = Instant::now();
        Ctl {
            snapshot,
            deadline: args.dev.hours.map(|h| now + Duration::from_secs_f64(h * 3600.0)),
            args,
            playlist: String::new(),
            playlist_wanted: true,
            switch_to: None,
            key_problem: config::stream_ready(&settings).err().map(|(_, reason)| reason),
            waiting_for_key: config::stream_ready(&settings).is_err(),
            playlist_problem: None,
            playlist_probe: None,
            settings,
            logs,
            tx,
            shared,
            yt,
            cache,
            videos: vec![],
            seams: vec![],
            enc: Encoder { child: None, media: 0.0, last_progress: now, started: now, restarts: 0, failures: 0, restart_at: None, ingest_was_down: false, speed: None, watch: media::WatchdogCfg::default() },
            cpu: Cpu::default(),
            watch: Watch::default(),
            live: None,
            metas: HashMap::new(),
            feeder_seq: 0,
            target: None,
            resolved: HashMap::new(),
            resolving: HashSet::new(),
            refresh_after: now,
            refresh_pending: None,
            spare: None,
            spare_due: None,
            filler_since: None,
            step_offset: BASE,
            break_url: None,
            jobs: jobs_tx,
            downloading: None,
            sizing: false,
            dl_after: now,
            token,
            step: "none",
            consecutive_skips: 0,
            hold: rules::SkipHold::default(),
            dl_fail_streak: 0,
            feeder_fails: 0,
            feeder_after: now,
            control_seq: 0,
            ever_played: false,
            stopping: false,
            started_at: unix_ms(),
            cache_bytes: 0,
            max_cache: 0,
            seam_rows: 0,
            counts: HashMap::new(),
            last_state: now,
            last_buffer: now,
            last_scan: now,
            breaker: rules::Breaker::default(),
            t0: now,
            last_resolve: None,
            probe_key: None,
            fake_bot: false,
            unavailable_streak: HashSet::new(),
        }
    }

    fn count(&mut self, k: &'static str) {
        *self.counts.entry(k).or_default() += 1;
    }

    fn n(&self) -> usize {
        self.videos.len()
    }

    fn view_timeline(&self) -> i64 {
        BASE + ticks(self.enc.media)
    }

    /// What the viewer sees: (index, P_view, source).
    fn view(&self) -> (usize, f64, Source) {
        match rules::view_at(&self.seams, self.view_timeline()) {
            // A seam from before a switch to a shorter list can name an index the list no longer has.
            Some((s, pos)) => (s.index % self.n().max(1), pos, s.source.clone()),
            None => (self.target.as_ref().map_or(0, |t| t.index), self.target.as_ref().map_or(0.0, |t| t.pos), Source::Slate),
        }
    }

    fn queue_end(&self) -> i64 {
        self.shared.q.lock().unwrap().end().unwrap_or(BASE)
    }

    fn read_ahead(&self) -> f64 {
        secs(self.queue_end() - self.view_timeline())
    }

    fn duration(&self, i: usize) -> Option<f64> {
        self.videos.get(i).and_then(|v| v.entry.duration)
    }

    fn id(&self, i: usize) -> String {
        self.videos.get(i).map_or(String::new(), |v| v.entry.id.clone())
    }

    fn file_of(&self, i: usize) -> PathBuf {
        self.cache.join(format!("{}.mp4", self.id(i)))
    }


    /// The now-playing filter script for a video feeder; none on the slate or a filler, or when the
    /// setting is off. A failed write is logged and the video airs without the overlay.
    fn overlay_for(&self, index: usize, source: &Source, pos: f64, filler: bool) -> Option<PathBuf> {
        if !self.settings.overlay || filler || *source == Source::Slate {
            return None;
        }
        let e = &self.videos.get(index)?.entry;
        match overlay::prepare(&e.title, e.duration, pos, &self.args.paths.data.join("overlay")) {
            Ok(p) => Some(p),
            Err(err) => {
                self.logs.event("warn", "overlay-failed", json!({"error": err.to_string()}));
                None
            }
        }
    }

    fn start_feeder(&mut self, index: usize, source: Source, pos: f64, reason: &'static str, filler: bool) {
        let file = self.file_of(index);
        let (resolved, expire) = match source {
            Source::Url => {
                let (r, at) = self.resolved.get(&index).cloned().expect("url feeder without a resolve");
                let e = expire_of(&r, at, self.args.dev.url_ttl, unix_ms() as u64 / 1000);
                (Some(r), e)
            }
            _ => (None, None),
        };
        let path = match source {
            Source::Slate => self.args.paths.slate.to_string_lossy().into_owned(),
            _ => file.to_string_lossy().into_owned(),
        };
        let input = match &resolved {
            Some(r) => Input::Url { video: &r.video, audio: &r.audio, headers: &r.headers },
            None => Input::File(&path),
        };
        self.feeder_seq += 1;
        self.refresh_pending = None;
        if self.spare.as_ref().is_some_and(|s| s.index != index) {
            self.spare = None;
        }
        if !filler && self.spare_due.is_none_or(|(i, _)| i != index) {
            self.spare_due = (source == Source::Url).then(|| (index, Instant::now() + Duration::from_secs_f64(rules::SPARE_AFTER_S)));
        }
        let seq = self.feeder_seq;
        if !filler {
            self.filler_since = None;
        } else if self.filler_since.is_none() {
            self.filler_since = Some(seq);
        }
        let offset = self.queue_end();
        if watchdog::moves_cut(reason) {
            self.watch.cut_at = self.watch.cut_at.max(secs(offset - BASE));
        }
        // Only a silence retry feeds as "watchdog". Taken before the spawn: a failed spawn is retried and overwrites it.
        if reason == "watchdog" && !filler {
            self.watch.retried = Some((index, secs(offset - BASE)));
        }
        let fault = self.watch.fault.filter(|(i, _)| *i == index && !filler && source != Source::Slate).map(|(_, f)| f);
        let overlay = self.overlay_for(index, &source, pos, filler);
        let spawned = procs::command(&self.args.paths.ffmpeg)
            .args(media::feeder_args(&input, rules::seek_for(&source, pos), offset, fault, overlay.as_deref()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                self.feeder_fails += 1;
                let (log, wait) = rules::feeder_spawn_failed(self.feeder_fails);
                if log {
                    self.logs.event("critical", "feeder-spawn-failed", json!({"error": e.to_string(), "index": index, "source": source.name()}));
                }
                self.feeder_after = Instant::now() + Duration::from_secs(wait);
                // The video waits for the next try instead of being lost; a slate is started when needed anyway.
                if !filler {
                    self.target = Some(Target { index, pos, reason, gated: false });
                }
                return;
            }
        };
        if std::mem::take(&mut self.feeder_fails) > 0 {
            self.logs.event("info", "feeder-spawn-recovered", json!({"index": index, "source": source.name()}));
        }
        let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let child = Arc::new(Mutex::new(child));
        self.shared.q.lock().unwrap().live = Some(Live { seq, last_pts: None, frames: 0, last_push: Instant::now(), blocked: false });
        let name = if source == Source::Slate { "slate".to_string() } else { self.id(index) };
        rules::push_seam(&mut self.seams, Seam { offset, index, source: source.clone(), start_pos: pos });
        self.metas.insert(seq, FeederMeta { name: name.clone(), started_iso: iso_now(), offset, index, source: source.clone(), start_pos: pos, reason });
        let logs = self.logs.clone();
        let tag = format!("[{seq} {name}]");
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let t = line.trim();
                let progress = t.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                if !t.is_empty() && !progress {
                    logs.process("feeder", seq, &format!("{} {tag} {t}\n", iso_now()));
                }
            }
        });
        let (shared, tx, c2) = (self.shared.clone(), self.tx.clone(), child.clone());
        std::thread::spawn(move || {
            let wait = || loop {
                if let Ok(Some(s)) = c2.lock().unwrap().try_wait() {
                    return s;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut status = None;
            let (frames, _, _) = media::pump(&shared, seq, offset, stdout, || {
                let s = wait();
                status = Some(s);
                s.success()
            });
            let status = status.unwrap_or_else(wait);
            let _ = tx.send(Msg::FeederExit { seq, code: status.code(), frames });
        });
        self.logs.event("info", "feeder-start", json!({"seq": seq, "name": name, "index": index, "source": source.name(), "pos": pos, "offset": secs(offset), "reason": reason, "fault": fault.map(|f| format!("{f:?}"))}));
        self.live = Some(Feeder { seq, child, index, source, start_pos: pos, offset, started: Instant::now(), filler, expire });
    }

    /// Stop the live feeder. Its unfinished last PES never reaches the queue (the trim).
    /// Returns (index, P_fed) of what it fed.
    fn cut_live(&mut self) -> Option<(usize, f64, Source)> {
        let f = self.live.take()?;
        let last = {
            let mut q = self.shared.q.lock().unwrap();
            let l = q.live.take();
            self.shared.cv.notify_all();
            l.and_then(|l| l.last_pts)
        };
        procs::kill_tree(f.child.lock().unwrap().id());
        let fed = last.map_or(f.start_pos, |p| rules::fed_pos(f.start_pos, f.offset, p + FRAME));
        Some((f.index, fed, f.source))
    }

    /// Drop the slate that is only covering a wait (every filler since the last real feeder),
    /// all but `keep` past the writer.
    fn cut_filler(&mut self, keep: i64) {
        if self.live.as_ref().is_some_and(|f| f.filler) {
            self.cut_live();
        }
        if let Some(seq) = self.filler_since.take() {
            self.shared.q.lock().unwrap().flush_keeping(seq, keep);
        }
    }

    fn next_playable(&mut self, from: usize) -> usize {
        let n = self.n();
        if let Some(i) = (0..n).map(|d| (from + d) % n).find(|&i| !self.videos[i].skipped) {
            return i;
        }
        // Every video failing at once is not the videos' fault: wait before going round again,
        // or a broken feeder or player client burns through the list every few seconds.
        let now = self.now_s();
        let wait = self.hold.trip(now);
        self.logs.event("critical", "all-skipped", json!({"holdS": wait, "level": self.hold.level}));
        self.videos.iter_mut().for_each(|v| v.skipped = false);
        from % n
    }

    fn set_target(&mut self, index: usize, pos: f64, reason: &'static str, gated: bool) {
        let n = self.n();
        if n == 0 {
            return;
        }
        // Just after a switch to a shorter list the view can still name an old, larger index.
        let index = index % n;
        let (index, pos) = rules::resolve_start(index, pos, self.duration(index), n);
        // A fresh video gets a fresh retry budget; retries, refreshes and restarts keep theirs.
        if matches!(reason, "normal" | "skip" | "control" | "coldstart") {
            if let Some(v) = self.videos.get_mut(index) {
                v.retries = 0;
            }
            (self.watch.fault, self.watch.retried) = (None, None);
        }
        self.target = Some(Target { index, pos, reason, gated });
    }

    /// Ladder levels 1 and 2 for the video a feeder or resolve failed on.
    fn fail(&mut self, index: usize, fed: f64, why: &str, detail: Value) {
        let retries = self.videos[index].retries;
        match rules::ladder_for(why, retries) {
            Ladder::Retry => {
                self.videos[index].retries += 1;
                self.renew_url(index);
                self.step = "retry";
                self.step_offset = self.queue_end();
                self.count("retries");
                self.logs.event("warn", "retry", json!({"index": index, "id": self.id(index), "why": why, "detail": detail, "retry": retries + 1, "pos": fed}));
                self.set_target(index, fed, "retry", false);
            }
            Ladder::Skip => {
                self.videos[index].skipped = true;
                self.consecutive_skips += 1;
                self.step = "skip";
                self.step_offset = self.queue_end();
                self.count("skips");
                let level = if self.consecutive_skips >= 3 { "critical" } else { "warn" };
                self.logs.event(level, "skip", json!({"index": index, "id": self.id(index), "why": why, "detail": detail, "consecutive": self.consecutive_skips}));
                let next = self.next_playable(index + 1);
                self.set_target(next, 0.0, "skip", false);
            }
        }
    }

    /// A retry plays from a new URL: the spare if usable, else a fresh resolve.
    fn renew_url(&mut self, index: usize) {
        let (now_s, id, margin) = (unix_ms() as u64 / 1000, self.id(index), self.margin_s());
        match retry_spare(&mut self.spare, &mut self.spare_due, index, &id, now_s, margin, self.args.dev.url_ttl, Instant::now()) {
            Some(r) => {
                self.logs.event("info", "spare-used", json!({"index": index, "id": id}));
                self.resolved.insert(index, (r, Instant::now()));
            }
            None => {
                self.resolved.remove(&index);
            }
        }
    }

    fn margin_s(&self) -> u64 {
        self.args.dev.refresh_margin.unwrap_or(1800)
    }

    fn now_s(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    /// One slot for a call to YouTube: the global resolve spacing, then the breaker.
    /// Some(true) = this call is the breaker's probe.
    fn youtube_slot(&mut self) -> Option<bool> {
        if self.last_resolve.is_some_and(|t| t.elapsed().as_secs_f64() < rules::RESOLVE_SPACING_S) {
            return None;
        }
        let now = self.now_s();
        match self.breaker.allow(now) {
            rules::Allow::Yes => Some(false),
            rules::Allow::Probe => Some(true),
            rules::Allow::No => None,
        }
    }

    /// Returns whether a resolve was started (or is already running).
    fn request_resolve(&mut self, index: usize, purpose: Purpose) -> bool {
        let key = (index, purpose == Purpose::Size);
        if self.resolving.contains(&key) {
            return true;
        }
        let Some(probe) = self.youtube_slot() else { return false };
        self.resolving.insert(key);
        self.last_resolve = Some(Instant::now());
        if probe {
            self.probe_key = Some(key);
            self.logs.event("info", "youtube-probe", json!({"index": index, "id": self.id(index), "level": self.breaker.level}));
        }
        let (yt, logs, tx, id) = (self.yt.clone(), self.logs.clone(), self.tx.clone(), self.id(index));
        if std::mem::take(&mut self.fake_bot) {
            // debug fault: answer as YouTube's bot check does, without asking YouTube
            let _ = tx.send(Msg::Resolved { index, id, purpose, result: Err("YouTube bot check: sign in to confirm you're not a bot (injected)".into()) });
            return true;
        }
        std::thread::spawn(move || {
            let result = yt.resolve(&id, &logs);
            let _ = tx.send(Msg::Resolved { index, id, purpose, result });
        });
        true
    }

    /// Feed a YouTube call's outcome to the breaker and act on a change.
    fn breaker_result(&mut self, ok: bool, bot: bool, counts: bool, probe: bool, reason: &str) {
        let now = self.now_s();
        let change = self.breaker.result(ok, bot, counts, probe, now);
        let retry_at = self.breaker_retry_iso();
        match change {
            rules::BreakerChange::Opened => {
                self.logs.event("critical", "youtube-breaker-open", json!({"reason": reason, "bot": bot, "retryAt": retry_at}));
                if let Some((i, _, cancel)) = &self.downloading {
                    self.logs.event("info", "download-cancel", json!({"index": i, "id": self.id(*i), "why": "breaker"}));
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            rules::BreakerChange::ProbeFailed => {
                self.logs.event("warn", "youtube-probe-failed", json!({"reason": reason, "level": self.breaker.level, "retryAt": retry_at}));
            }
            rules::BreakerChange::Closed => self.logs.event("info", "youtube-breaker-closed", json!({})),
            rules::BreakerChange::None => {}
        }
    }

    fn breaker_retry_iso(&self) -> Option<String> {
        self.breaker.open.then(|| iso(unix_ms() + ((self.breaker.retry_at - self.now_s()).max(0.0) * 1000.0) as u128))
    }

    fn fresh_url(&self, i: usize) -> bool {
        self.resolved.get(&i).is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(600))
    }

    /// Fresh and past `available_at`: googlevideo 403s a URL used in the first seconds after
    /// the resolve (yt-dlp itself sleeps for it before downloading).
    fn url_ready(&self, i: usize) -> bool {
        let now = unix_ms() as u64 / 1000;
        self.fresh_url(i) && self.resolved.get(&i).is_some_and(|(r, _)| r.available_at.is_none_or(|t| now > t))
    }


    fn start_encoder(&mut self) {
        let watch = watchdog_cfg(&self.settings);
        let mut child = match procs::command(&self.args.paths.ffmpeg)
            .args(match media::encoder_args(&config::target_url(&self.settings), &self.settings.encoder, &watch) {
                Ok(a) => a,
                Err(e) => {
                    self.logs.event("critical", "encoder-spawn-failed", json!({"error": e}));
                    self.enc.restart_at = Some(Instant::now() + Duration::from_secs(5));
                    return;
                }
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                self.logs.event("critical", "encoder-spawn-failed", json!({"error": e.to_string()}));
                self.enc.restart_at = Some(Instant::now() + Duration::from_secs(5));
                return;
            }
        };
        let generation = {
            let mut q = self.shared.q.lock().unwrap();
            q.stdin = Some((q.generation, child.stdin.take().unwrap()));
            self.shared.cv.notify_all();
            q.generation
        };
        let (stderr, logs, tx) = (child.stderr.take().unwrap(), self.logs.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let (mut out_us, mut speed) = (None::<String>, String::new());
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                encoder_stderr_line(&logs, &tx, generation, &line, &mut out_us, &mut speed);
            }
        });
        if self.counts.contains_key("encoderStarts") {
            self.enc.restarts += 1;
        }
        self.count("encoderStarts");
        self.enc.child = Some(child);
        self.enc.watch = watch;
        self.enc.media = 0.0;
        self.enc.speed = None;
        self.enc.started = Instant::now();
        self.enc.last_progress = Instant::now();
        self.enc.restart_at = None;
        self.logs.event("info", "encoder-start", json!({"generation": generation, "restarts": self.enc.restarts}));
    }

    fn encoder_down(&mut self, why: &str) {
        let (index, pos, _) = self.view();
        let mut child = self.enc.child.take().unwrap();
        procs::kill_tree(child.id());
        let code = child.wait().ok().and_then(|s| s.code());
        self.cut_live();
        {
            let mut q = self.shared.q.lock().unwrap();
            q.flush_from(0);
            q.written_pts = None;
            q.view = BASE;
            q.generation += 1;
            self.shared.cv.notify_all();
        }
        self.seams.clear();
        // A new encoder starts its media clock at 0: nothing judged on the old one carries over.
        (self.watch.freeze, self.watch.silence, self.watch.cut_at, self.watch.retried) = (None, None, 0.0, None);
        self.enc.speed = None;
        self.enc.failures += 1;
        let wait = rules::encoder_backoff_s(self.enc.failures - 1);
        self.enc.restart_at = Some(Instant::now() + Duration::from_secs(wait));
        self.enc.ingest_was_down = false;
        self.step = "encoder-restart";
        self.step_offset = BASE;
        self.count("encoderExits");
        let level = if self.enc.failures >= 3 { "critical" } else { "warn" };
        self.logs.event(level, "encoder-exit", json!({"why": why, "code": code, "failures": self.enc.failures, "restartInS": wait, "resumeIndex": index, "resumePos": pos}));
        // A control (or playlist switch) waiting to be fed is what the viewer asked for next.
        if !self.stopping && self.target.as_ref().is_none_or(|t| t.reason != "control") {
            self.set_target(index, pos, "encoder-restart", false);
        }
    }

    /// Is the RTMP ingest accepting connections? Only probed while the encoder is down.
    fn ingest_up(&self) -> bool {
        let target = config::target_url(&self.settings);
        let hostport = target.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
        use std::net::ToSocketAddrs;
        let addr = hostport.to_socket_addrs().ok().and_then(|mut a| a.next());
        addr.is_some_and(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
    }


    /// Every 10 s: CPU of the encoder, the live feeder and the whole machine, as one encoder.csv row.
    fn sample_cpu(&mut self) {
        if self.cpu.last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return;
        }
        self.cpu.last = Some(Instant::now());
        let feeder_pid = self.live.as_ref().map(|f| f.child.lock().unwrap().id());
        let enc_pid = self.enc.child.as_ref().map(|c| c.id());
        let Some((idle, total)) = procs::system_times() else {
            self.cpu.prev = None;
            return;
        };
        let enc = enc_pid.and_then(procs::cpu_times).unwrap_or(0);
        let feed = feeder_pid.and_then(procs::cpu_times).unwrap_or(0);
        if let Some(p) = self.cpu.prev {
            let dt = total.saturating_sub(p.total);
            // A process that replaced the previous one has no baseline: 0 for this window.
            self.cpu.encoder = rules::window_cpu(p.enc_pid, enc_pid, p.enc, enc, dt);
            self.cpu.feeder = rules::window_cpu(p.feeder_pid, feeder_pid, p.feeder, feed, dt);
            // The machine's load does not depend on the encoder being up.
            self.cpu.system = 100.0 - rules::cpu_percent(idle.saturating_sub(p.idle), dt);
            let (media, gen) = (self.enc.media, self.shared.q.lock().unwrap().generation);
            self.logs.csv("encoder", &[
                unix_ms().to_string(), ((media * 1e6) as i64).to_string(), self.enc.speed.map_or(String::new(), |s| format!("{s}x")), gen.to_string(),
                self.enc.speed.map_or(String::new(), |s| s.to_string()),
                format!("{:.2}", self.cpu.encoder), format!("{:.2}", self.cpu.feeder), format!("{:.2}", self.cpu.system),
            ]);
        }
        self.cpu.prev = Some(CpuMark { enc_pid, feeder_pid, enc, feeder: feed, idle, total });
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.sample_cpu();
        if self.deadline.is_some_and(|d| now >= d) && !self.stopping {
            self.logs.event("info", "deadline", json!({"hours": self.args.dev.hours}));
            self.shutdown();
        }
        // The playlist fetch is a YouTube call too: same spacing, never past an open breaker.
        // With no list yet, the fetch is also the open breaker's probe.
        if let Some(probe) = self.playlist_wanted.then(|| self.youtube_slot()).flatten() {
            self.playlist_wanted = false;
            self.last_resolve = Some(Instant::now());
            let (yt, l, tx, id) = (self.yt.clone(), self.logs.clone(), self.tx.clone(), self.settings.playlist_id.clone());
            if probe {
                self.playlist_probe = Some(id.clone());
            }
            std::thread::spawn(move || {
                let result = yt.playlist(&format!("https://www.youtube.com/playlist?list={id}"), &l);
                let _ = tx.send(Msg::Playlist(id, result));
            });
        }

        // Ladder level 3: the encoder.
        if let Some(c) = self.enc.child.as_mut() {
            if let Ok(Some(s)) = c.try_wait() {
                self.encoder_down(&format!("exited ({s})"));
            } else if self.enc.last_progress.elapsed() > STALL {
                self.encoder_down("no progress for 10 s");
            } else if self.enc.failures > 0 && self.enc.last_progress.elapsed() < Duration::from_secs(1) && self.enc.started.elapsed() > Duration::from_secs(30) {
                self.enc.failures = 0;
            }
        }
        if self.enc.child.is_none() {
            let due = self.enc.restart_at.is_some_and(|t| now >= t);
            // A dead ingest that comes back restarts at once instead of sitting out the backoff.
            let back = if self.enc.restart_at.is_some() {
                let up = self.ingest_up();
                let back = up && self.enc.ingest_was_down;
                self.enc.ingest_was_down |= !up;
                back
            } else {
                false
            };
            if due || back {
                self.start_encoder();
            }
            return;
        }

        self.watchdog_work();

        // The live feeder: stall, handover, refresh.
        if let Some(f) = &self.live {
            let (stalled, frames) = {
                let q = self.shared.q.lock().unwrap();
                q.live.as_ref().map_or((false, 0), |l| (!l.blocked && l.last_push.elapsed() > STALL, l.frames))
            };
            let (index, source, filler, started, expire) = (f.index, f.source.clone(), f.filler, f.started, f.expire);
            if stalled {
                let (i, fed, _) = self.cut_live().unwrap();
                if filler {
                    self.logs.event("warn", "slate-stall", json!({}));
                } else {
                    self.fail(i, fed, "stall", json!({"frames": frames}));
                }
            } else if source == Source::Url && !filler && self.videos[index].on_disk {
                let (i, fed, _) = self.cut_live().unwrap();
                self.count("handovers");
                self.logs.event("info", "handover", json!({"index": i, "id": self.id(i), "pos": fed}));
                self.start_feeder(i, Source::File, fed, "handover", false);
            } else if source == Source::Url && !filler && self.refresh_pending == Some(index) && self.url_ready(index) {
                // URL-to-URL handover at P_fed on the continuing offset, before the old URL expires.
                self.refresh_pending = None;
                let (i, fed, _) = self.cut_live().unwrap();
                self.count("refreshes");
                self.start_feeder(i, Source::Url, fed, "refresh", false);
            } else if source == Source::Url && !filler && self.refresh_pending.is_none() && started.elapsed() > Duration::from_secs(30) && now >= self.refresh_after {
                let unix = unix_ms() as u64 / 1000;
                if expire.is_some_and(|e| rules::refresh_due(unix, e, self.margin_s())) && !self.resolving.contains(&(index, false)) {
                    if self.request_resolve(index, Purpose::Refresh) {
                        self.logs.event("info", "url-refresh", json!({"index": index, "id": self.id(index), "expire": expire, "now": unix}));
                        self.refresh_after = now + Duration::from_secs(30);
                    }
                }
            }
            if let Some(f) = &self.live {
                if !f.filler && f.started.elapsed() > Duration::from_secs(300) {
                    let i = f.index;
                    self.videos[i].retries = 0;
                    self.hold.healthy();
                }
            }
            self.spare_work(now);
        }

        // The target: what should be fed next.
        if let Some(mut t) = self.target.clone().filter(|_| !self.hold.holding(self.now_s())) {
            if self.live.as_ref().is_none_or(|f| f.filler) && !self.videos.is_empty() {
                // YouTube is off limits: play a cached file instead of waiting on the slate.
                if self.breaker.open && !self.videos[t.index].on_disk {
                    let n = self.n();
                    if let Some(j) = (1..n).map(|d| (t.index + d) % n).find(|&j| self.videos[j].on_disk && !self.videos[j].skipped) {
                        self.logs.event("warn", "breaker-retarget", json!({"from": t.index, "to": j, "id": self.id(j)}));
                        t = Target { index: j, pos: 0.0, ..t };
                        self.target = Some(t.clone());
                    }
                }
                let on_disk = self.videos[t.index].on_disk;
                if !on_disk && !self.fresh_url(t.index) {
                    self.request_resolve(t.index, Purpose::Play);
                }
                let gate_open = !t.gated || self.read_ahead() < self.settings.read_ahead_seconds;
                if gate_open && now >= self.feeder_after && (on_disk || self.url_ready(t.index)) {
                    let source = if on_disk { Source::File } else { Source::Url };
                    self.cut_filler(rules::cut_margin(&source, t.pos));
                    self.target = None;
                    self.start_feeder(t.index, source, t.pos, t.reason, false);
                }
            }
        }

        // Nothing to feed and the pipe running dry: the slate covers it.
        if self.live.is_none() && self.read_ahead() < FILLER_BELOW_S && now >= self.feeder_after {
            let (index, pos, reason) = self.target.as_ref().map_or((self.view().0, 0.0, "coldstart"), |t| (t.index, t.pos, t.reason));
            self.start_feeder(index, Source::Slate, pos, reason, true);
        }

        let on_video = self.view().2 != Source::Slate;
        if !self.ever_played && on_video {
            self.ever_played = true;
        }
        // Recovered once the viewer has watched 10 s of video past the point of the fault.
        if self.step != "none" && on_video && self.view_timeline() > self.step_offset + 10 * rules::HZ {
            self.step = "none";
        }
        // An open breaker probes once per step even when nothing needs a resolve.
        if self.breaker.open && !self.breaker.probing && self.now_s() >= self.breaker.retry_at && !self.videos.is_empty() {
            let i = self.target.as_ref().map_or(self.view().0, |t| t.index);
            self.request_resolve(i, Purpose::Probe);
        }
        self.cache_work();
        self.periodic();
    }


    /// The output watchdog: judge each open freeze or silence once it has lasted its duration,
    /// and heal. A switch or retry in progress is left alone; its new seam is judged on its own.
    fn watchdog_work(&mut self) {
        if self.target.as_ref().is_some_and(|t| t.reason != "normal") {
            return;
        }
        let now = self.enc.media;
        for freeze in [true, false] {
            let (open, need, kind) = if freeze {
                (self.watch.freeze, self.enc.watch.freeze_s, "freeze")
            } else {
                (self.watch.silence, self.enc.watch.silence_s, "silence")
            };
            let Some((since, reported)) = open else { continue };
            let Some(start) = watchdog::due(since.max(self.watch.cut_at), &self.seams, now, need) else { continue };
            let verdict = watchdog::judge(&if freeze { Detect::FreezeStart(start) } else { Detect::SilenceStart(start) }, &self.seams, now);
            let (index, pos) = match verdict {
                Verdict::Ignore => {
                    if !reported {
                        let (index, pos, _) = self.view();
                        self.watchdog_event(kind, index, pos, start, "ignored");
                        let open = if freeze { &mut self.watch.freeze } else { &mut self.watch.silence };
                        *open = Some((since, true));
                    }
                    continue;
                }
                Verdict::Freeze { index, pos } | Verdict::Silence { index, pos } => (index, pos),
            };
            if self.videos.get(index).is_none() {
                continue; // a seam from before a playlist switch
            }
            let heal = watchdog::heal(&verdict, since, self.watch.retried, self.videos[index].retries).unwrap();
            self.watch.fault = None;
            self.count(if freeze { "watchdogFreezes" } else { "watchdogSilences" });
            if heal == Heal::Content {
                // Silent again right after a clean restart: the silence is the video's. Report it once, play on.
                self.watch.silence = None;
                self.watchdog_event(kind, index, pos, start, "content");
                continue;
            }
            let at = self.cut_for_heal(index, pos);
            self.watchdog_event(kind, index, pos, start, if heal == Heal::Skip { "skip" } else { "retry" });
            if freeze {
                // The stall ladder: its retry budget, its new URL, its skip.
                self.fail(index, at, "freeze", json!({"since": start, "pos": pos}));
            } else {
                self.step = "retry";
                self.step_offset = self.queue_end();
                self.set_target(index, at, "watchdog", false);
            }
            return; // one heal per tick: the other stretch is judged against the new cut
        }
    }

    fn watchdog_event(&mut self, kind: &str, index: usize, pos: f64, since: f64, outcome: &str) {
        let v = json!({"verdict": kind, "index": index, "id": self.id(index), "pos": round2(pos), "since": round2(since), "outcome": outcome, "at": iso_now()});
        let level = if matches!(outcome, "ignored" | "content") { "info" } else { "warn" };
        self.logs.event(level, &format!("watchdog-{kind}"), v.clone());
        self.watch.last = v;
    }

    /// A heal replaces what is on air now: cut the live feeder and drop the queue past the cut
    /// margin. Returns the video position at the cut, where the restart continues.
    fn cut_for_heal(&mut self, index: usize, pos: f64) -> f64 {
        self.cut_live();
        // The kept margin is on air whatever it is; a later cut_filler only drops a new slate.
        self.filler_since = None;
        let source = if self.videos[index].on_disk { Source::File } else { Source::Url };
        self.shared.q.lock().unwrap().flush_keeping(0, rules::cut_margin(&source, pos));
        let cut = self.queue_end();
        self.watch.cut_at = secs(cut - BASE);
        match rules::view_at(&self.seams, cut) {
            Some((s, p)) if s.index == index && s.source != Source::Slate => p,
            _ => pos,
        }
    }

    /// Keep a spare URL for the video on air from its URL, through the same gate as every resolve.
    fn spare_work(&mut self, now: Instant) {
        let Some(index) = self.live.as_ref().filter(|f| f.source == Source::Url && !f.filler).map(|f| f.index) else { return };
        if self.resolving.contains(&(index, false)) {
            return;
        }
        let id = self.id(index);
        let wanted = spare_wanted(self.spare_due, index, now, self.spare.as_ref(), &id, unix_ms() as u64 / 1000, self.args.dev.url_ttl, self.margin_s());
        if wanted && self.request_resolve(index, Purpose::Spare) {
            self.logs.event("info", "spare-resolve", json!({"index": index, "id": id}));
        }
    }

    fn window(&self) -> Vec<usize> {
        let sizes: Vec<Option<u64>> = self.videos.iter().map(|v| if v.on_disk { Some(v.disk_bytes) } else { v.bytes }).collect();
        let skipped: Vec<bool> = self.videos.iter().map(|v| v.skipped || v.dl_failed).collect();
        rules::window(self.view().0, &sizes, &skipped, self.settings.cache_budget_mb * 1_048_576)
    }

    fn cache_work(&mut self) {
        if self.videos.is_empty() || self.stopping {
            return;
        }
        let win = self.window();
        let view = self.view().0;
        let fed = self.live.as_ref().filter(|f| !f.filler).map(|f| f.index);
        let url_fed = self.live.as_ref().filter(|f| f.source == Source::Url && !f.filler).map(|f| f.index);
        let target = self.target.as_ref().map(|t| t.index);
        // The video on air from its URL, or about to be, is downloaded first and never deleted.
        let first = url_fed.or(target).filter(|&i| !self.videos[i].on_disk && !self.videos[i].dl_failed);

        // Cancel a download that no longer belongs to the look-ahead, or that blocks the first one.
        if let Some((i, _, cancel)) = &self.downloading {
            let wanted = win.contains(i) || Some(*i) == url_fed || Some(*i) == target;
            let wrong = !wanted || first.is_some_and(|u| u != *i);
            if wrong && !cancel.load(Ordering::Relaxed) {
                self.logs.event("info", "download-cancel", json!({"index": i, "id": self.id(*i)}));
                cancel.store(true, Ordering::Relaxed);
            }
        }

        // Deletion by playout: anything outside the window that no feeder reads.
        for i in 0..self.n() {
            if !rules::deletable(i, &win, view, fed) || Some(i) == target {
                continue;
            }
            self.videos[i].dl_failed = false;
            if !self.videos[i].on_disk || self.downloading.as_ref().is_some_and(|d| d.0 == i) {
                continue;
            }
            let file = self.file_of(i);
            let bytes = self.videos[i].disk_bytes;
            if std::fs::remove_file(&file).is_ok() || !file.exists() {
                self.videos[i].on_disk = false;
                self.videos[i].disk_bytes = 0;
                // Where this video's last airing ended on the timeline, if it aired this encoder generation.
                let end = self.seams.iter().rposition(|s| s.index == i && s.source != Source::Slate).and_then(|k| self.seams.get(k + 1)).map(|s| format!("{:.6}", secs(s.offset))).unwrap_or_default();
                self.logs.csv("deletes", &[iso_now(), self.id(i), bytes.to_string(), format!("{:.6}", secs(self.view_timeline())), end]);
            }
        }

        // "prev" lands on a video that playout already deleted: keep its URL resolved and past
        // available_at, so the cut does not wait for yt-dlp plus the site's delay.
        let prev = (view + self.n() - 1) % self.n();
        if !self.videos[prev].on_disk && !self.fresh_url(prev) && !self.videos[prev].skipped {
            self.request_resolve(prev, Purpose::Play);
        }

        // Dispatch one job when the downloader is idle.
        if self.downloading.is_some() || self.sizing || Instant::now() < self.dl_after || self.breaker.open {
            return;
        }
        let on_disk: Vec<bool> = self.videos.iter().map(|v| v.on_disk).collect();
        let failed: Vec<bool> = self.videos.iter().map(|v| v.dl_failed || v.skipped).collect();
        let pick = first.filter(|&u| !failed[u]).or_else(|| rules::next_download(&win, &on_disk, &failed));
        let Some(i) = pick else { return };
        if self.videos[i].bytes.is_none() && i != view && Some(i) != first {
            if self.youtube_slot().is_none() {
                return;
            }
            self.last_resolve = Some(Instant::now());
            self.sizing = true;
            let _ = self.jobs.send(Job::Size(i, self.id(i)));
        } else {
            let cancel = Arc::new(AtomicBool::new(false));
            self.downloading = Some((i, 0.0, cancel.clone()));
            let _ = self.jobs.send(Job::Download(i, self.id(i), cancel));
        }
    }

    fn periodic(&mut self) {
        if self.last_scan.elapsed() >= Duration::from_secs(1) {
            self.last_scan = Instant::now();
            self.cache_bytes = std::fs::read_dir(&self.cache).map(|d| d.filter_map(|e| e.ok()?.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
            self.max_cache = self.max_cache.max(self.cache_bytes);
        }
        let (index, pos, source) = self.view();
        let switching = self.target.as_ref().is_some_and(|t| t.reason != "normal") || self.step != "none" || source == Source::Slate || self.enc.child.is_none();
        if self.last_state.elapsed() >= Duration::from_secs(2) && !switching && !self.videos.is_empty() && !self.stopping {
            self.last_state = Instant::now();
            let v = json!({"index": index, "id": self.id(index), "position": (pos * 1000.0).round() / 1000.0, "savedAt": iso_now()});
            let file = self.state_file();
            let tmp = file.with_extension("tmp");
            if std::fs::write(&tmp, v.to_string()).is_ok() {
                let _ = std::fs::rename(&tmp, &file);
            }
        }
        if self.last_buffer.elapsed() >= Duration::from_secs(10) {
            self.last_buffer = Instant::now();
            let playing = if source == Source::Slate { "slate".into() } else { self.id(index) };
            let (dl_id, pct) = self.downloading.as_ref().map_or((String::new(), String::new()), |d| (self.id(d.0), format!("{:.1}", d.1)));
            self.logs.csv(
                "buffer",
                &[iso_now(), self.cache_bytes.to_string(), self.videos.iter().filter(|v| v.on_disk).count().to_string(), dl_id, pct, playing, format!("{:.1}", self.enc.media), format!("{:.2}", self.read_ahead())],
            );
            self.save_run();
        }
    }


    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Control { cmd, index, id, reason, reply } => {
                let index = id.and_then(|id| self.position(&id)).map(|i| i as i64).or(index);
                let _ = reply.send(self.control(&cmd, index, reason));
            }
            Msg::Fault { kind, count, reply } => {
                let _ = reply.send(self.fault(&kind, count));
            }
            Msg::Shutdown => self.shutdown(),
            Msg::Reload { index, id, reply } => {
                let _ = reply.send(self.reload(index, id));
            }
            Msg::Playlist(id, result) => self.on_playlist(id, result),
            Msg::Resolved { index, id, purpose, result } => self.on_resolved(index, &id, purpose, result),
            Msg::FeederExit { seq, code, frames } => self.on_feeder_exit(seq, code, frames),
            Msg::EncProgress { generation, media, speed } => {
                let mut q = self.shared.q.lock().unwrap();
                if generation == q.generation && self.enc.child.is_some() {
                    if media > self.enc.media {
                        self.enc.last_progress = Instant::now();
                    }
                    self.enc.media = media;
                    self.enc.speed = speed.or(self.enc.speed);
                    q.view = BASE + ticks(media);
                    self.shared.cv.notify_all();
                }
            }
            Msg::Detect { generation, d } => {
                // A line from an earlier encoder belongs to a timeline that is gone.
                if generation != self.shared.q.lock().unwrap().generation || self.enc.child.is_none() {
                    return;
                }
                match d {
                    Detect::FreezeStart(t) => self.watch.freeze = Some((t, false)),
                    Detect::FreezeEnd(_) => self.watch.freeze = None,
                    Detect::SilenceStart(t) => self.watch.silence = Some((t, false)),
                    Detect::SilenceEnd(_) => self.watch.silence = None,
                    Detect::Rebuilt { video: true } => self.watch.freeze = None,
                    Detect::Rebuilt { video: false } => self.watch.silence = None,
                }
            }
            Msg::DlProgress { index, percent } => {
                if let Some(d) = self.downloading.as_mut().filter(|d| d.0 == index) {
                    d.1 = percent;
                }
            }
            Msg::Downloaded { mut index, id, ok, cancelled, bytes, ms, reason } => {
                self.downloading = None;
                if !cancelled {
                    self.breaker_result(ok, rules::is_bot_check(&reason), false, false, &reason);
                }
                if self.id(index) != id {
                    // finished across a playlist switch
                    match self.videos.iter().position(|v| v.entry.id == id) {
                        Some(j) => index = j,
                        None => {
                            let _ = std::fs::remove_file(self.cache.join(format!("{id}.mp4")));
                            return;
                        }
                    }
                }
                let v = &mut self.videos[index];
                let result = if cancelled {
                    "cancel"
                } else if ok {
                    v.on_disk = true;
                    v.disk_bytes = bytes;
                    v.bytes = Some(bytes);
                    v.dl_attempts = 0;
                    self.dl_fail_streak = 0;
                    "done"
                } else {
                    v.dl_attempts += 1;
                    self.dl_after = Instant::now() + Duration::from_secs(5);
                    if v.dl_attempts >= 2 {
                        v.dl_failed = true;
                        v.dl_attempts = 0;
                        "skip"
                    } else {
                        "retry"
                    }
                };
                if result == "skip" {
                    // A video that cannot be downloaded plays from its URL; many in a row is systemic
                    // (the merge, the token server, a player client) and needs a human.
                    self.dl_fail_streak += 1;
                    self.logs.event("warn", "download-failed", json!({"index": index, "id": self.id(index), "reason": reason, "inARow": self.dl_fail_streak}));
                    if self.dl_fail_streak == DOWNLOAD_FAILS_CRITICAL {
                        self.logs.event("critical", "downloads-failing", json!({"inARow": self.dl_fail_streak, "reason": reason}));
                    }
                }
                let attempt = self.videos[index].dl_attempts;
                self.logs.csv("downloads", &[iso_now(), self.id(index), result.into(), attempt.to_string(), bytes.to_string(), ms.to_string(), reason]);
            }
        }
    }

    fn on_playlist(&mut self, id: String, result: Result<Vec<Entry>, String>) {
        let (mut entries, mut reason) = match result {
            Ok(e) => (e, String::new()),
            Err(r) => (vec![], r),
        };
        entries.drain(..self.args.dev.start.min(entries.len()));
        if self.args.dev.max_videos > 0 {
            entries.truncate(self.args.dev.max_videos);
        }
        if entries.is_empty() && reason.is_empty() {
            reason = "the playlist has no entries (after --start/--max-videos)".into();
        }
        // An empty list counts as a failure too, or a refetch would run every 5 s.
        let probe = self.playlist_probe.as_ref() == Some(&id);
        if probe {
            self.playlist_probe = None;
        }
        self.breaker_result(!entries.is_empty(), rules::is_bot_check(&reason), true, probe, &reason);
        let first = match playlist_action(self.videos.is_empty(), &self.playlist, &id, &self.settings.playlist_id) {
            PlaylistAction::Drop => return,
            action => action == PlaylistAction::Load,
        };
        if entries.is_empty() {
            let f = playlist_failure(first, self.playlist_problem.is_some());
            if let Some(level) = f.event {
                // a switch keeps playing the old list
                self.logs.event(level, "playlist-failed", json!({"playlistId": id, "reason": reason, "playing": self.playlist, "refetch": f.refetch}));
            }
            if f.refetch {
                self.playlist_problem = Some(reason);
                self.playlist_wanted = true;
            }
            return;
        }
        if self.playlist_problem.take().is_some() {
            self.logs.event("info", "playlist-recovered", json!({"playlistId": id}));
        }
        self.logs.write("playlist.json", &serde_json::to_string_pretty(&entries.iter().map(|e| json!({"id": e.id, "title": e.title, "duration": e.duration})).collect::<Vec<_>>()).unwrap());
        let new: Vec<Video> = entries.into_iter().map(|entry| Video { entry, bytes: None, on_disk: false, disk_bytes: 0, dl_attempts: 0, dl_failed: false, skipped: false, retries: 0 }).collect();
        let old = std::mem::replace(&mut self.videos, new);
        self.playlist = id.clone();
        self.logs.event("info", "playlist", json!({"playlistId": id, "count": self.n()}));
        if !first {
            // A switch: indexes now mean other videos. Keep files the new list has, drop the rest.
            for v in old.iter().filter(|v| v.on_disk) {
                match self.videos.iter_mut().find(|n| n.entry.id == v.entry.id) {
                    Some(n) => (n.on_disk, n.disk_bytes, n.bytes) = (true, v.disk_bytes, Some(v.disk_bytes)),
                    None => {
                        let _ = std::fs::remove_file(self.cache.join(format!("{}.mp4", v.entry.id)));
                    }
                }
            }
            if let Some((_, _, cancel)) = &self.downloading {
                cancel.store(true, Ordering::Relaxed);
            }
            self.resolved.clear();
            (self.spare, self.spare_due) = (None, None);
            (self.refresh_pending, self.break_url, self.consecutive_skips) = (None, None, 0);
            self.unavailable_streak.clear();
            let start = self.switch_start().unwrap_or(0);
            if let Err(e) = self.control("jump", Some(start as i64), None) {
                self.logs.event("warn", "playlist-switch-failed", json!({"reason": e}));
            }
            return;
        }
        // A switch asked for before any list loaded names its own start; state.json is the old list's.
        if let Some(start) = self.switch_start() {
            self.set_target(start, 0.0, "coldstart", false);
            return;
        }
        let resume = std::fs::read_to_string(self.state_file()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
        match resume {
            Some(s) => {
                let id = s["id"].as_str().unwrap_or("");
                let index = self.videos.iter().position(|v| v.entry.id == id).or(s["index"].as_u64().map(|i| i as usize).filter(|&i| i < self.n())).unwrap_or(0);
                let pos = s["position"].as_f64().unwrap_or(0.0);
                self.logs.event("info", "resume", json!({"index": index, "id": self.id(index), "pos": pos}));
                self.set_target(index, pos, "resume", false);
            }
            None => self.set_target(0, 0.0, "coldstart", false),
        }
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.videos.iter().position(|v| v.entry.id == id)
    }

    /// Takes the pending switch's start in the loaded list: the id's position when the list has
    /// it (Node's index can count videos this list leaves out), else the index clamped to the list.
    fn switch_start(&mut self) -> Option<usize> {
        let (index, id) = self.switch_to.take()?;
        Some(id.and_then(|id| self.position(&id)).unwrap_or(index.min(self.n().saturating_sub(1))))
    }

    fn state_file(&self) -> PathBuf {
        self.args.paths.data.join("state.json")
    }

    /// POST /reload. Live fields apply now; ingest, key and encoder wait for a restart, except
    /// in needs-stream-key, where nothing runs yet and a working key starts the encoder. `index` and
    /// `id` say where a switch to a new playlist starts; with the same playlist they are ignored.
    fn reload(&mut self, index: Option<usize>, id: Option<String>) -> Result<Value, String> {
        let mut new = read_settings(&self.args)?;
        self.logs.add_key(&new.stream_key);
        let ready = config::stream_ready(&new);
        let old = &self.settings;
        if ready.is_err() {
            // An unusable new key changes nothing about the key that runs.
            (new.stream_key, new.key_error) = (old.stream_key.clone(), old.key_error.clone());
        }
        let plan = config::reload_plan(old, &new);
        let waiting = self.waiting_for_key;
        let running = if waiting { new } else { Settings { ingest_url: old.ingest_url.clone(), stream_key: old.stream_key.clone(), encoder: old.encoder.clone(), ..new } };
        if running.read_ahead_seconds != old.read_ahead_seconds {
            self.shared.q.lock().unwrap().ahead = ticks(running.read_ahead_seconds);
            self.shared.cv.notify_all();
        }
        *self.yt.player_client.lock().unwrap() = running.player_client.clone();
        let switch = running.playlist_id != old.playlist_id;
        if switch {
            // Fetch unless the new id is the list already playing (A, B, back to A before B lands).
            self.playlist_wanted = self.videos.is_empty() || running.playlist_id != self.playlist;
            self.switch_to = Some((index.unwrap_or(0), id));
        }
        self.settings = running;
        let restart = !waiting && plan == config::Reload::Restart;
        let key = key_after_reload(waiting, self.key_problem.is_some(), ready);
        self.logs.event("info", "reload", json!({"plan": format!("{plan:?}"), "restartRequired": restart, "playlistSwitch": switch, "streamKeyProblem": key.problem}));
        if let (Some((level, kind)), Some(reason)) = (key.event, &key.problem) {
            self.logs.event(level, kind, json!({"reason": reason, "running": "the running key carries on until the supervisor restarts"}));
        }
        self.key_problem = key.problem.clone();
        if key.start_encoder {
            self.waiting_for_key = false;
            self.start_encoder();
        }
        Ok(json!({"applied": true, "restartRequired": restart, "streamKeyProblem": key.problem}))
    }

    fn on_resolved(&mut self, index: usize, id: &str, purpose: Purpose, result: Result<Resolved, String>) {
        let key = (index, purpose == Purpose::Size);
        self.resolving.remove(&key);
        if purpose == Purpose::Size {
            self.sizing = false;
        }
        let probe = self.probe_key == Some(key);
        if probe {
            self.probe_key = None;
        }
        let reason = result.as_ref().err().cloned().unwrap_or_default();
        let per_video = rules::is_video_unavailable(&reason);
        let counts = rules::unavailable_counts(&mut self.unavailable_streak, result.is_ok(), per_video, index);
        self.breaker_result(result.is_ok(), rules::is_bot_check(&reason), counts, probe, &reason);
        if self.id(index) != id {
            return; // resolved across a playlist switch
        }
        let mut r = match result {
            Ok(r) => r,
            Err(reason) => {
                self.logs.event("warn", "resolve-failed", json!({"index": index, "id": self.id(index), "purpose": format!("{purpose:?}"), "reason": reason}));
                match purpose {
                    Purpose::Size => {
                        self.videos[index].dl_failed = true;
                        self.logs.csv("downloads", &[iso_now(), self.id(index), "skip".into(), "1".into(), "0".into(), "0".into(), reason]);
                    }
                    // The video itself will not play: the ladder skips it, breaker open or not.
                    Purpose::Play | Purpose::Probe if per_video => match self.target.clone().filter(|t| t.index == index) {
                        Some(t) => self.fail(index, t.pos, "unavailable", json!({"reason": reason})),
                        // Not on air or next (the "prev" pre-resolve): skipped, so it is not asked for again.
                        None => {
                            self.videos[index].skipped = true;
                            self.logs.event("warn", "skip", json!({"index": index, "id": self.id(index), "why": "unavailable", "detail": {"reason": reason}}));
                        }
                    },
                    // Not the video's fault while YouTube is off limits: no retry, no skip, the
                    // target waits (slate or a cached file) until the breaker closes.
                    Purpose::Play if self.breaker.open => {}
                    Purpose::Play => {
                        if let Some(t) = self.target.clone().filter(|t| t.index == index) {
                            self.fail(index, t.pos, "resolve", json!({"reason": reason}));
                        }
                    }
                    // Try again in a minute, not at the next free slot.
                    Purpose::Spare => delay_spare(&mut self.spare_due, index, Instant::now()),
                    Purpose::Refresh | Purpose::Probe => {}
                }
                return;
            }
        };
        if let Some((i, count)) = self.break_url.as_mut().filter(|(i, _)| *i == index) {
            r.video = rules::mangle_sig(&r.video);
            r.audio = rules::mangle_sig(&r.audio);
            self.logs.event("info", "fault-applied", json!({"fault": "break-url", "index": *i}));
            if let Some(c) = count {
                *c -= 1;
                if *c == 0 {
                    self.break_url = None;
                }
            }
        }
        if let Some(v) = self.videos.get_mut(index) {
            v.bytes = v.bytes.or(r.bytes);
            v.entry.duration = r.duration.or(v.entry.duration);
        }
        if purpose == Purpose::Spare {
            if self.target.as_ref().is_some_and(|t| t.index == index) && !self.fresh_url(index) {
                // A retry is already waiting on this video: the spare is its resolve.
                self.resolved.insert(index, (r, Instant::now()));
            } else if self.live.as_ref().is_some_and(|f| f.index == index && !f.filler) {
                store_spare(&mut self.spare, &mut self.spare_due, Spare { index, id: id.to_string(), r, at: Instant::now() }, Instant::now());
            }
            return;
        }
        self.resolved.insert(index, (r, Instant::now()));
        if purpose == Purpose::Refresh {
            self.refresh_pending = Some(index);
        }
    }

    fn on_feeder_exit(&mut self, seq: u64, code: Option<i32>, frames: u64) {
        if let Some(m) = self.metas.remove(&seq) {
            self.seam_rows += 1;
            self.logs.csv(
                "seams",
                &[
                    seq.to_string(), m.name, m.started_iso, format!("{:.6}", secs(m.offset)), frames.to_string(),
                    code.map_or("killed".into(), |c| c.to_string()), iso_now(), m.index.to_string(), m.source.name().into(),
                    format!("{:.3}", m.start_pos), m.reason.into(),
                ],
            );
        }
        if self.live.as_ref().is_none_or(|f| f.seq != seq) {
            return; // cut on purpose; already handled
        }
        let f = self.live.as_ref().unwrap();
        let (index, filler, source, start_pos, offset) = (f.index, f.filler, f.source.clone(), f.start_pos, f.offset);
        let last = self.shared.q.lock().unwrap().live.as_ref().and_then(|l| l.last_pts);
        self.cut_live();
        let fed = last.map_or(start_pos, |p| rules::fed_pos(start_pos, offset, p + FRAME));
        if filler || self.stopping {
            return;
        }
        let ok = code == Some(0);
        if rules::ended_early(ok, fed, self.duration(index)) {
            self.fail(index, fed, if ok { "early-eof" } else { "exit" }, json!({"code": code, "frames": frames, "fed": fed, "duration": self.duration(index), "source": source.name()}));
        } else {
            self.consecutive_skips = 0;
            // A video played to its end (short videos never reach the 5 min mark).
            self.hold.healthy();
            if self.target.is_none() {
                let next = self.next_playable(index + 1);
                self.set_target(next, 0.0, "normal", true);
            }
        }
    }


    /// Build outside the lock, then swap, so the API thread never waits on status building.
    fn publish_status(&self) {
        let v = self.status();
        *self.snapshot.lock().unwrap() = v;
    }

    fn status(&self) -> Value {
        let (index, pos, source) = self.view();
        let mut head = status_head(
            self.stopping,
            self.waiting_for_key,
            self.key_problem.as_deref(),
            self.playlist_problem.as_deref().filter(|_| self.videos.is_empty()),
            self.ever_played,
            self.enc.child.is_none() || self.step != "none",
        );
        let v = self.videos.get(index);
        // One lock per statement: json! keeps its temporaries (and their guards) alive to the end.
        let token = { let t = self.token.lock().unwrap(); json!({"state": t.state, "restarts": t.restarts}) };
        let generation = self.shared.q.lock().unwrap().generation;
        let mut body = json!({
            "playing": {"index": index, "id": v.map(|v| v.entry.id.clone()), "title": v.map(|v| v.entry.title.clone()), "source": source.name(), "position": (pos * 1000.0).round() / 1000.0, "duration": v.and_then(|v| v.entry.duration), "spareReady": self.spare.as_ref().is_some_and(|s| s.index == index)},
            "timeline": secs(self.view_timeline()),
            "readAheadSeconds": (self.read_ahead() * 100.0).round() / 100.0,
            "cacheBytes": self.cache_bytes,
            "downloading": self.downloading.as_ref().map(|d| json!({"index": d.0, "percent": d.1})),
            "encoder": {"alive": self.enc.child.is_some(), "restarts": self.enc.restarts, "generation": generation, "mediaTime": self.enc.media, "speed": self.enc.speed, "cpuPercent": round2(self.cpu.encoder)},
            "cpu": {"encoder": round2(self.cpu.encoder), "feeder": round2(self.cpu.feeder), "system": round2(self.cpu.system)},
            "tokenServer": token,
            "recovery": {"step": self.step, "retries": v.map_or(0, |v| v.retries)},
            "watchdog": {"last": self.watch.last},
            "playlistCount": self.n(),
            "hold": self.hold.holding(self.now_s()).then(|| json!({"reason": "every video in the playlist failed; the slate airs until the next try", "level": self.hold.level, "until": iso(unix_ms() + ((self.hold.until.unwrap_or(0.0) - self.now_s()).max(0.0) * 1000.0) as u128)})),
            "youtube": {"breaker": if self.breaker.open { "open" } else { "closed" }, "retryAt": self.breaker_retry_iso(), "level": self.breaker.level, "failures": self.breaker.failures},
        });
        if let (Some(h), Some(b)) = (head.as_object_mut(), body.as_object_mut()) {
            h.append(b);
        }
        head
    }

    /// `reason` is the caller's note for `restart-encoder`; the other commands ignore it.
    fn control(&mut self, cmd: &str, index: Option<i64>, reason: Option<String>) -> Result<Value, String> {
        if self.stopping {
            return Err("stopping".into());
        }
        if cmd == "restart-encoder" {
            if self.enc.child.is_none() {
                return if self.waiting_for_key {
                    Err("needs-stream-key: no encoder runs until a reload brings a working key".into())
                } else {
                    Ok(json!({"accepted": true, "alreadyRestarting": true}))
                };
            }
            self.logs.event("info", "encoder-restart-requested", json!({"reason": reason}));
            self.encoder_down("restart requested");
            return Ok(json!({"accepted": true}));
        }
        let view = self.view().0;
        let t = rules::control_target(cmd, index, view, self.n())?;
        self.cut_live();
        // The kept second is on air whatever it is; a later cut_filler only drops a new slate.
        self.filler_since = None;
        let source = if self.videos[t].on_disk { Source::File } else { Source::Url };
        let flushed = self.shared.q.lock().unwrap().flush_keeping(0, rules::cut_margin(&source, 0.0));
        let cut = self.queue_end();
        self.control_seq += 1;
        self.count("controls");
        self.videos[t].skipped = false;
        self.videos[t].retries = 0;
        self.hold.until = None;
        self.step = "none";
        self.set_target(t, 0.0, "control", false);
        self.logs.csv("controls", &[unix_ms().to_string(), cmd.into(), index.map_or(String::new(), |i| i.to_string()), t.to_string(), self.id(t), format!("{:.6}", secs(cut)), flushed.to_string()]);
        self.logs.event("info", "control", json!({"cmd": cmd, "seq": self.control_seq, "from": view, "target": t, "id": self.id(t), "cutOffset": secs(cut), "flushedBytes": flushed}));
        self.tick();
        Ok(json!({"accepted": true, "seq": self.control_seq, "targetIndex": t, "targetId": self.id(t), "cutOffset": secs(cut)}))
    }

    fn fault(&mut self, kind: &str, count: Option<u32>) -> Result<Value, String> {
        self.logs.event("info", "fault", json!({"fault": kind, "count": count}));
        match kind {
            "kill-feeder" | "stall-feeder" => {
                let f = self.live.as_ref().ok_or("no live feeder")?;
                let child = f.child.lock().unwrap();
                if kind == "kill-feeder" {
                    procs::kill_tree(child.id());
                } else if !procs::suspend(&child) {
                    return Err("suspend failed".into());
                }
            }
            "break-url" => {
                let index = self.live.as_ref().filter(|f| !f.filler).map(|f| f.index).or(self.target.as_ref().map(|t| t.index)).ok_or("nothing playing")?;
                self.break_url = Some((index, count));
                self.resolved.remove(&index);
                self.spare = None;
            }
            "freeze-video" | "mute-audio" => {
                let index = self.live.as_ref().filter(|f| !f.filler).map(|f| f.index).ok_or("no live video feeder")?;
                let fault = if kind == "freeze-video" { media::FeederFault::Freeze } else { media::FeederFault::Mute };
                let (_, fed, _) = self.cut_live().unwrap();
                // The restart at P_fed picks the fault up in start_feeder and keeps it until the watchdog acts.
                self.set_target(index, fed, "fault", false);
                self.watch.fault = Some((index, fault));
            }
            "kill-encoder" => {
                procs::kill_tree(self.enc.child.as_ref().ok_or("no encoder")?.id());
            }
            "kill-token-server" => self.token.lock().unwrap().kill = true,
            "bot-check" => {
                self.fake_bot = true;
                let i = self.view().0;
                self.request_resolve(i, Purpose::Probe);
            }
            _ => return Err(format!("unknown fault {kind:?}")),
        }
        Ok(json!({"accepted": true, "kind": kind}))
    }

    fn save_run(&self) {
        let (a, d, st) = (&self.args, &self.args.dev, &self.settings);
        let token = { let t = self.token.lock().unwrap(); json!({"state": t.state, "restarts": t.restarts}) };
        let v = json!({
            "startedAt": self.started_at, "endedAt": if self.stopping { Some(unix_ms()) } else { None },
            "root": a.paths.root, "data": a.paths.data, "config": a.config,
            "target": config::target_url(st), "encoder": st.encoder, "baseOffset": secs(BASE),
            "readAheadSeconds": st.read_ahead_seconds, "playlistId": st.playlist_id, "budgetBytes": st.cache_budget_mb * 1_048_576,
            "playerClient": st.player_client, "hours": d.hours, "limitRate": d.limit_rate, "start": d.start, "maxVideos": d.max_videos,
            "urlTtlS": d.url_ttl, "urlRefreshMarginS": d.refresh_margin, "debugFaults": a.debug_faults, "entries": self.n(),
            "seams": self.seam_rows, "encoderExitedEarly": self.counts.contains_key("encoderExits"),
            "maxCacheBytes": self.max_cache, "counts": self.counts,
            "skipped": self.videos.iter().filter(|v| v.skipped).map(|v| v.entry.id.clone()).collect::<Vec<_>>(),
            "tokenServer": token,
        });
        self.logs.write("run.json", &serde_json::to_string_pretty(&v).unwrap());
    }

    fn shutdown(&mut self) {
        self.shutdown_code(0);
    }

    fn shutdown_code(&mut self, code: i32) {
        self.stopping = true;
        self.logs.event("info", "shutdown", json!({"code": code}));
        // The process exits before the pumps report, so the live feeder's seams row is written here.
        let frames = self.shared.q.lock().unwrap().live.as_ref().map_or(0, |l| l.frames);
        let live = self.live.as_ref().map(|f| f.seq);
        self.cut_live();
        if let Some(seq) = live {
            self.on_feeder_exit(seq, None, frames);
        }
        if let Some((_, _, c)) = &self.downloading {
            c.store(true, Ordering::Relaxed);
        }
        self.token.lock().unwrap().stop = true;
        // Let the writer drop the encoder's stdin so it flushes and ends the RTMP stream cleanly.
        {
            let mut q = self.shared.q.lock().unwrap();
            q.shutdown = true;
            self.shared.cv.notify_all();
        }
        if let Some(mut c) = self.enc.child.take() {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until && matches!(c.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(100));
            }
            procs::kill_tree(c.id());
        }
        std::thread::sleep(Duration::from_millis(500)); // downloader and token watchdog see their flags
        self.save_run();
        std::process::exit(code);
    }
}


fn downloader(jobs: Receiver<Job>, yt: Arc<Ytdlp>, logs: Logs, tx: Sender<Msg>, cache: PathBuf) {
    for job in jobs {
        match job {
            Job::Size(index, id) => {
                let result = yt.resolve(&id, &logs);
                let _ = tx.send(Msg::Resolved { index, id, purpose: Purpose::Size, result });
            }
            Job::Download(index, id, cancel) => {
                let started = Instant::now();
                let file = cache.join(format!("{id}.mp4"));
                let mut last = -1.0;
                let out = yt.download(&id, &file, &logs, &cancel, |_| {}, |p| {
                    if p - last >= 1.0 || p >= 100.0 {
                        last = p;
                        let _ = tx.send(Msg::DlProgress { index, percent: p });
                    }
                });
                let ok = out.ok && !out.cancelled && file.exists();
                // yt-dlp leaves .part and per-format files behind; only the merged file stays.
                for e in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
                    let p = e.path();
                    if e.file_name().to_string_lossy().starts_with(&id) && (p != file || !ok) {
                        let _ = std::fs::remove_file(&p);
                    }
                }
                let bytes = if ok { std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) } else { 0 };
                let reason = if ok || out.cancelled { String::new() } else { ytdlp::describe_error(&out.stderr, None) };
                let _ = tx.send(Msg::Downloaded { index, id, ok, cancelled: out.cancelled, bytes, ms: started.elapsed().as_millis(), reason });
            }
        }
    }
}

const TOKEN_PORT: u16 = 4416;

/// Adopt a running PO-token server, or own one; replace it when it dies or hangs.
fn token_watchdog(state: Arc<Mutex<TokenState>>, paths: Paths, logs: Logs) {
    let mut owned: Option<Child> = None;
    let spawn = |logs: &Logs| match ytdlp::spawn_token_server(&paths.node, &paths.token_server, TOKEN_PORT, logs) {
        Ok(c) => {
            logs.event("info", "token-server-spawn", json!({"pid": c.id()}));
            Some(c)
        }
        Err(e) => {
            logs.event("critical", "token-server-spawn-failed", json!({"error": e.to_string()}));
            None
        }
    };
    if ytdlp::ping(TOKEN_PORT) {
        state.lock().unwrap().state = "adopted";
        logs.event("info", "token-server-adopt", json!({"port": TOKEN_PORT}));
    } else {
        owned = spawn(&logs);
        state.lock().unwrap().state = if owned.is_some() { "owned" } else { "down" };
    }
    let mut next_ping = Instant::now() + Duration::from_secs(15);
    let mut failed_pings = 0;
    let mut backoff_level = 0;
    let mut restart_at: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let (kill, stop) = {
            let mut s = state.lock().unwrap();
            (std::mem::take(&mut s.kill), s.stop)
        };
        if stop {
            if let Some(c) = owned.take() {
                procs::kill_tree(c.id());
            }
            return;
        }
        if kill {
            match owned.as_mut() {
                Some(c) => {
                    procs::kill_tree(c.id());
                }
                None => {
                    if let Some(pid) = procs::pid_on_port(TOKEN_PORT) {
                        procs::kill_tree(pid);
                    }
                }
            }
        }
        let exited = owned.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(Some(_))));
        if Instant::now() >= next_ping && restart_at.is_none() {
            if ytdlp::ping(TOKEN_PORT) {
                failed_pings = 0;
                backoff_level = 0;
                next_ping = Instant::now() + Duration::from_secs(15);
            } else {
                failed_pings += 1;
                // The second ping comes sooner, so a dead adopted server is replaced well within 35 s.
                next_ping = Instant::now() + Duration::from_secs(5);
            }
        }
        if restart_at.is_none() && (exited || failed_pings >= 2) {
            let wait = rules::token_backoff_s(backoff_level);
            logs.event("warn", "token-server-down", json!({"exited": exited, "failedPings": failed_pings, "restartInS": wait}));
            state.lock().unwrap().state = "down";
            if let Some(mut c) = owned.take() {
                procs::kill_tree(c.id());
                let _ = c.wait();
            }
            restart_at = Some(Instant::now() + Duration::from_secs(wait));
        }
        if restart_at.is_some_and(|t| Instant::now() >= t) {
            restart_at = None;
            backoff_level += 1;
            failed_pings = 0;
            owned = spawn(&logs);
            let mut s = state.lock().unwrap();
            s.restarts += 1;
            s.state = if owned.is_some() { "owned" } else { "down" };
            logs.event("warn", "token-server-restart", json!({"restarts": s.restarts}));
            next_ping = Instant::now() + Duration::from_secs(15);
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().is_some_and(|a| a == "protect") {
        match protect_stdin() {
            Ok(blob) => {
                // Node reads the blob from stdout: a failed write is a failed protect.
                if writeln!(std::io::stdout().lock(), "{blob}").is_err() {
                    std::process::exit(1);
                }
            }
            Err(e) => {
                logs::echo(&mut std::io::stderr().lock(), &format!("streamloop-supervisor protect: {e}"));
                std::process::exit(1);
            }
        }
        return;
    }
    if argv.first().is_some_and(|a| a == "launch") {
        let rest = &argv[1..];
        let logs = parse_args(rest).ok().map(|a| a.paths.data.join("logs"));
        let code = launch::run(rest, PORT, |breakaway, pid| {
            if let Some(dir) = &logs {
                let _ = std::fs::create_dir_all(dir);
                Logs::new(dir.clone(), "").event("info", "launched", json!({"breakaway": breakaway, "pid": pid}));
            }
        });
        std::process::exit(code);
    }
    let started = parse_args(&argv).and_then(|a| {
        let s = read_settings(&a)?;
        Ok((a, s))
    });
    let (args, settings) = match started {
        Ok(v) => v,
        Err(e) => {
            logs::echo(&mut std::io::stderr().lock(), &format!("streamloop-supervisor: {e}"));
            std::process::exit(2);
        }
    };
    // Bind before anything else: a duplicate launch must exit 2 before it prunes logs, wipes the
    // cache or starts a thread or child, all of which the live instance owns.
    let (tx, rx) = channel::<Msg>();
    let snapshot: api::Snapshot = Arc::new(Mutex::new(json!({"state": "starting", "encoder": {"alive": false}})));
    if let Err(e) = api::serve(PORT, args.debug_faults, tx.clone(), snapshot.clone()) {
        logs::echo(&mut std::io::stderr().lock(), &format!("streamloop-supervisor: {e}"));
        std::process::exit(2);
    }
    if let Err(e) = procs::enter_job() {
        logs::echo(&mut std::io::stderr().lock(), &format!("warning: {e}; children will not die with the supervisor"));
    }
    let logs = Logs::new(args.paths.data.join("logs"), &settings.stream_key);
    let _ = std::fs::create_dir_all(&logs.dir);
    logs.prune();
    // ponytail: files from an earlier run are not adopted, so the cache starts empty; adopt them if restarts re-download too much.
    let cache = args.paths.data.join("cache");
    let _ = std::fs::remove_dir_all(&cache);
    let _ = std::fs::create_dir_all(&cache);

    let yt = Arc::new(Ytdlp::new(&args.paths, &settings.player_client, args.dev.limit_rate.clone()));
    let mut queue = Queue::new(192 * 1024 * 1024);
    queue.ahead = ticks(settings.read_ahead_seconds);
    let shared = Arc::new(Shared { q: Mutex::new(queue), cv: Condvar::new() });
    let token = Arc::new(Mutex::new(TokenState { state: "down", restarts: 0, kill: false, stop: false }));
    let (jobs_tx, jobs_rx) = channel::<Job>();

    {
        let s = shared.clone();
        std::thread::spawn(move || media::writer(&s));
    }
    {
        let (t, p, l) = (token.clone(), args.paths.clone(), logs.clone());
        std::thread::spawn(move || token_watchdog(t, p, l));
    }
    {
        let (yt, l, tx, c) = (yt.clone(), logs.clone(), tx.clone(), cache.clone());
        std::thread::spawn(move || downloader(jobs_rx, yt, l, tx, c));
    }

    let mut ctl = Ctl::new(args, settings, logs, tx, shared, yt, cache, jobs_tx, token, snapshot);
    let p = &ctl.args.paths;
    ctl.logs.event("info", "start", json!({"target": config::target_url(&ctl.settings), "encoder": ctl.settings.encoder, "root": p.root, "data": p.data, "ffmpeg": p.ffmpeg, "deno": p.deno}));
    match config::stream_ready(&ctl.settings) {
        Ok(()) => ctl.start_encoder(),
        // The API and token server stay up so Node and the dashboard can show it.
        Err((kind, reason)) => ctl.logs.event("critical", kind, json!({"reason": reason})),
    }
    ctl.save_run();
    let mut last_tick = Instant::now();
    let mut log_day = logs::local_date();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(msg) => {
                ctl.handle(msg);
                ctl.publish_status();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => unreachable!("the control loop holds a sender"),
        }
        if last_tick.elapsed() >= Duration::from_millis(100) {
            last_tick = Instant::now();
            ctl.tick();
            ctl.publish_status();
        }
        let today = logs::local_date();
        if today != log_day {
            log_day = today;
            ctl.logs.prune();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control loop over `ids` with no encoder, feeder or yt-dlp behind it; logs go to a temp folder.
    fn test_ctl(name: &str, ids: &[&str]) -> Ctl {
        let dir = std::env::temp_dir().join(format!("sl-ctl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let d = dir.to_string_lossy().into_owned();
        let args = parse_args(&["--root".into(), d.clone(), "--data".into(), d]).unwrap();
        let logs = Logs::new(dir.join("logs"), "");
        let yt = Arc::new(Ytdlp::new(&args.paths, "web", None));
        let shared = Arc::new(Shared { q: Mutex::new(Queue::new(1 << 20)), cv: Condvar::new() });
        let token = Arc::new(Mutex::new(TokenState { state: "down", restarts: 0, kill: false, stop: false }));
        let (tx, _) = channel();
        let (jobs, _) = channel();
        let mut ctl = Ctl::new(args, Settings::default(), logs, tx, shared, yt, dir.join("cache"), jobs, token, Arc::default());
        ctl.playlist_wanted = false;
        ctl.videos = ids.iter().map(|id| Video { entry: Entry { id: id.to_string(), title: id.to_string(), duration: Some(100.0) }, bytes: None, on_disk: false, disk_bytes: 0, dl_attempts: 0, dl_failed: false, skipped: false, retries: 0 }).collect();
        ctl
    }

    #[test]
    fn an_unavailable_video_is_skipped_at_once_without_opening_the_breaker() {
        let mut ctl = test_ctl("unavailable", &["a", "b", "c"]);
        ctl.set_target(0, 0.0, "coldstart", false);
        let reason = "ERROR: [youtube] a: Private video. Sign in if you've been granted access to this video";

        ctl.on_resolved(0, "a", Purpose::Play, Err(reason.into()));

        assert_eq!((ctl.breaker.open, ctl.breaker.failures), (false, 0), "a private video counted toward the breaker");
        assert!(ctl.videos[0].skipped);
        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.reason)), Some((1, "skip")), "the next video is not up");
    }

    #[test]
    fn a_bot_check_on_a_play_resolve_still_opens_the_breaker_and_skips_nothing() {
        let mut ctl = test_ctl("botcheck", &["a", "b"]);
        ctl.set_target(0, 0.0, "coldstart", false);

        ctl.on_resolved(0, "a", Purpose::Play, Err("ERROR: [youtube] a: Sign in to confirm you're not a bot".into()));

        assert!(ctl.breaker.open);
        assert!(!ctl.videos[0].skipped);
        assert_eq!(ctl.target.as_ref().map(|t| t.index), Some(0), "the target waits for the breaker");
    }

    #[test]
    fn an_unavailable_video_that_is_not_the_target_is_skipped_so_it_is_not_resolved_again() {
        let mut ctl = test_ctl("unavailable-prev", &["a", "b", "c"]);
        ctl.set_target(1, 0.0, "coldstart", false);

        ctl.on_resolved(0, "a", Purpose::Play, Err("ERROR: [youtube] a: Video unavailable. This video has been removed by the uploader".into()));

        assert!(ctl.videos[0].skipped);
        assert_eq!(ctl.breaker.failures, 0);
        assert_eq!(ctl.target.as_ref().map(|t| t.index), Some(1));
    }

    #[test]
    fn all_skipped_holds_the_target_off_and_a_control_lifts_the_hold() {
        let mut ctl = test_ctl("all-skipped", &["a", "b"]);
        ctl.videos[0].skipped = true;
        ctl.set_target(1, 0.0, "coldstart", false);
        ctl.videos[1].retries = rules::MAX_RETRIES;

        ctl.fail(1, 0.0, "exit", json!({}));

        let now = ctl.now_s();
        assert!(ctl.hold.holding(now), "no hold after every video was skipped");
        assert!(ctl.status()["hold"]["reason"].is_string(), "{}", ctl.status());
        ctl.control("next", None, None).unwrap();
        assert!(!ctl.hold.holding(now), "a control did not lift the hold");
    }

    fn exited_child() -> Child {
        std::process::Command::new("cmd").args(["/c", "exit 1"]).stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap()
    }

    #[test]
    fn an_encoder_restart_right_after_a_switch_to_a_shorter_list_targets_a_video_that_exists() {
        let mut ctl = test_ctl("short-list", &["a", "b"]);
        // the viewer is still on video 5 of the old, longer list
        ctl.seams.push(Seam { offset: BASE, index: 5, source: Source::File, start_pos: 12.0 });
        ctl.enc.child = Some(exited_child());

        ctl.encoder_down("test");

        let t = ctl.target.clone().expect("no target after the encoder went down");
        assert!(t.index < ctl.n(), "target {} on a list of {}", t.index, ctl.n());
    }

    /// A control loop playing PLa whose config.json now names `next`; the fetch of `next` has not landed.
    fn reload_ctl(name: &str, next: &str) -> Ctl {
        let mut ctl = test_ctl(name, &["a", "b"]);
        (ctl.playlist, ctl.settings.playlist_id) = ("PLa".into(), "PLa".into());
        std::fs::create_dir_all(ctl.args.config.parent().unwrap()).unwrap();
        std::fs::write(&ctl.args.config, json!({"playlists": [{"id": next}], "supervisor": {}}).to_string()).unwrap();
        ctl
    }

    fn entries(ids: &[&str]) -> Result<Vec<Entry>, String> {
        Ok(ids.iter().map(|id| Entry { id: id.to_string(), title: id.to_string(), duration: Some(100.0) }).collect())
    }

    #[test]
    fn a_reload_to_another_playlist_with_an_index_starts_at_that_video() {
        let mut ctl = reload_ctl("reload-index", "PLb");

        ctl.reload(Some(3), None).unwrap();
        ctl.on_playlist("PLb".into(), entries(&["p", "q", "r", "s", "t"]));

        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.reason)), Some((3, "control")));
    }

    #[test]
    fn a_reload_index_past_the_end_of_the_new_playlist_clamps_to_its_last_video() {
        let mut ctl = reload_ctl("reload-clamp", "PLb");

        ctl.reload(Some(9), None).unwrap();
        ctl.on_playlist("PLb".into(), entries(&["p", "q", "r"]));

        assert_eq!(ctl.target.as_ref().map(|t| t.index), Some(2));
    }

    #[test]
    fn a_reload_to_another_playlist_without_an_index_starts_at_video_0() {
        let mut ctl = reload_ctl("reload-no-index", "PLb");

        ctl.reload(None, None).unwrap();
        ctl.on_playlist("PLb".into(), entries(&["p", "q", "r", "s"]));

        assert_eq!(ctl.target.as_ref().map(|t| t.index), Some(0));
    }

    #[test]
    fn a_reload_of_the_same_playlist_ignores_the_index() {
        let mut ctl = reload_ctl("reload-same", "PLa");
        ctl.set_target(1, 4.0, "coldstart", false);

        ctl.reload(Some(0), Some("b".into())).unwrap();

        assert!(!ctl.playlist_wanted, "the same playlist was fetched again");
        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.reason)), Some((1, "coldstart")));
        assert_eq!(ctl.control_seq, 0, "the index jumped");
    }

    #[test]
    fn a_reload_id_found_in_the_new_playlist_wins_over_the_index() {
        let mut ctl = reload_ctl("reload-id", "PLb");

        // Node's row 3 counted a private video the supervisor's list leaves out
        ctl.reload(Some(3), Some("s".into())).unwrap();
        ctl.on_playlist("PLb".into(), entries(&["p", "q", "s", "t"]));

        assert_eq!(ctl.target.as_ref().map(|t| t.index), Some(2));
    }

    #[test]
    fn a_reload_id_missing_from_the_new_playlist_falls_back_to_the_index() {
        let mut ctl = reload_ctl("reload-unknown-id", "PLb");

        ctl.reload(Some(1), Some("gone".into())).unwrap();
        ctl.on_playlist("PLb".into(), entries(&["p", "q", "r"]));

        assert_eq!(ctl.target.as_ref().map(|t| t.index), Some(1));
    }

    #[test]
    fn a_switch_before_the_first_playlist_loads_starts_at_its_video_not_the_resume_point() {
        let mut ctl = reload_ctl("reload-first", "PLb");
        (ctl.videos, ctl.playlist) = (vec![], String::new());
        std::fs::write(ctl.state_file(), json!({"id": "q", "index": 1, "position": 30.0}).to_string()).unwrap();

        ctl.reload(Some(3), None).unwrap();
        ctl.on_playlist("PLb".into(), entries(&["p", "q", "r", "s", "t"]));

        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.pos)), Some((3, 0.0)));
    }

    /// Sends a control through the loop's message handler, as the HTTP API does.
    fn send_control(ctl: &mut Ctl, cmd: &str, index: Option<i64>, id: Option<&str>) -> Result<Value, String> {
        let (reply, rx) = channel();
        ctl.handle(Msg::Control { cmd: cmd.into(), index, id: id.map(String::from), reason: None, reply });
        rx.recv().unwrap()
    }

    #[test]
    fn a_jump_by_id_lands_on_that_video_when_the_index_counts_a_video_the_list_left_out() {
        // Node's list had a private video at 1, so "c" is its row 3; here "c" is at 2
        let mut ctl = test_ctl("jump-id", &["a", "b", "c"]);

        let reply = send_control(&mut ctl, "jump", Some(3), Some("c")).unwrap();

        assert_eq!((reply["targetIndex"].clone(), reply["targetId"].clone()), (json!(2), json!("c")));
    }

    #[test]
    fn a_jump_with_an_unknown_id_falls_back_to_the_index() {
        let mut ctl = test_ctl("jump-unknown-id", &["a", "b", "c"]);

        assert_eq!(send_control(&mut ctl, "jump", Some(1), Some("gone")).unwrap()["targetIndex"], json!(1));
        assert!(send_control(&mut ctl, "jump", Some(5), Some("gone")).is_err(), "an out-of-range index still errors");
        assert_eq!(send_control(&mut ctl, "jump", Some(0), None).unwrap()["targetIndex"], json!(0));
    }

    #[test]
    fn an_encoder_restart_keeps_a_pending_control_target() {
        let mut ctl = test_ctl("keep-control", &["a", "b", "c"]);
        ctl.seams.push(Seam { offset: BASE, index: 0, source: Source::File, start_pos: 12.0 });
        ctl.set_target(2, 0.0, "control", false);
        ctl.enc.child = Some(exited_child());

        ctl.encoder_down("test");

        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.reason)), Some((2, "control")));
    }

    #[test]
    fn restart_encoder_control_takes_the_encoder_down_path() {
        let mut ctl = test_ctl("restart-encoder", &["a", "b", "c"]);
        ctl.seams.push(Seam { offset: BASE, index: 1, source: Source::File, start_pos: 12.0 });
        (ctl.enc.child, ctl.waiting_for_key) = (Some(exited_child()), false);

        let reply = ctl.control("restart-encoder", None, Some("twitch shows the channel offline".into()));

        assert_eq!(reply, Ok(json!({"accepted": true})));
        assert!(ctl.enc.child.is_none() && ctl.enc.restart_at.is_some(), "the encoder is not down with a restart scheduled");
        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.pos, t.reason)), Some((1, 12.0, "encoder-restart")), "does not resume at P_view");
        let ev = events(&ctl);
        assert!(ev.contains(&("info".into(), "encoder-restart-requested".into())) && ev.iter().any(|(_, k)| k == "encoder-exit"), "{ev:?}");
        let log = std::fs::read_dir(&ctl.logs.dir).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("events-")).unwrap();
        assert!(std::fs::read_to_string(log.path()).unwrap().contains("twitch shows the channel offline"));
        assert_eq!(ctl.control("restart-encoder", None, None), Ok(json!({"accepted": true, "alreadyRestarting": true})), "a restart is already pending");
        ctl.waiting_for_key = true;
        assert!(ctl.control("restart-encoder", None, None).is_err(), "no encoder until a key arrives");
    }

    /// Video 1 on air from timeline 0 at position 0; the stretch began at 5 s, the encoder is at `media`.
    fn watchdog_ctl(name: &str, freeze: bool, media: f64) -> Ctl {
        let mut ctl = test_ctl(name, &["a", "b", "c"]);
        ctl.seams.push(Seam { offset: BASE, index: 1, source: Source::File, start_pos: 0.0 });
        ctl.enc.media = media;
        if freeze { ctl.watch.freeze = Some((5.0, false)) } else { ctl.watch.silence = Some((5.0, false)) }
        ctl
    }

    /// The retry's feeder went on air where the queue ends: its seam is pushed as `start_feeder` would.
    fn retry_aired(ctl: &mut Ctl) -> f64 {
        let t = ctl.target.take().expect("no retry target");
        let offset = ctl.queue_end();
        ctl.seams.push(Seam { offset, index: t.index, source: Source::File, start_pos: t.pos });
        secs(offset - BASE)
    }

    #[test]
    fn a_silence_is_reported_as_content_and_never_cuts_the_stream() {
        let mut ctl = watchdog_ctl("wd-silence", false, 40.0);
        ctl.shared.q.lock().unwrap().written_pts = Some(BASE + ticks(50.0));

        ctl.watchdog_work();

        assert_eq!((ctl.watch.last["outcome"].as_str(), ctl.watch.last["verdict"].as_str()), (Some("content"), Some("silence")), "{}", ctl.watch.last);
        assert_eq!(events(&ctl).last(), Some(&("info".into(), "watchdog-silence".into())));
        assert!(ctl.target.is_none(), "a silence changed the target");
        assert!(!ctl.videos[1].skipped);
    }

    #[test]
    fn a_freeze_still_there_after_its_retries_is_skipped() {
        let mut ctl = watchdog_ctl("wd-freeze", true, 30.0);
        ctl.shared.q.lock().unwrap().written_pts = Some(BASE + ticks(40.0));
        let mut outcomes = vec![];

        for _ in 0..3 {
            ctl.watchdog_work();
            outcomes.push(ctl.watch.last["outcome"].as_str().unwrap_or("").to_string());
            if ctl.videos[1].skipped {
                break;
            }
            let aired = retry_aired(&mut ctl);
            ctl.enc.media = aired + 5.0 + 20.0 + 1.0;
            ctl.shared.q.lock().unwrap().written_pts = Some(BASE + ticks(ctl.enc.media + 2.0));
        }

        assert_eq!(outcomes, ["retry", "retry", "skip"]);
        assert!(ctl.videos[1].skipped);
        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.reason)), Some((2, "skip")));
    }

    /// The tree-kill run of 2026-10-25: 21 s of slate came out frozen, then the video's bt709 tags
    /// rebuilt the filter graph, which resets freezedetect, so no freeze_end ever came.
    #[test]
    fn a_slate_freeze_cut_off_by_a_filter_graph_rebuild_does_not_carry_into_the_video() {
        let mut ctl = test_ctl("wd-rebuild", &["a", "b"]);
        ctl.seams.push(Seam { offset: BASE, index: 0, source: Source::Slate, start_pos: 0.0 });
        ctl.seams.push(Seam { offset: BASE + ticks(21.0), index: 0, source: Source::Url, start_pos: 0.0 });
        ctl.enc.child = Some(exited_child());
        let generation = ctl.shared.q.lock().unwrap().generation;
        let feed = |ctl: &mut Ctl, line: &str| {
            let (tx, rx) = channel();
            encoder_stderr_line(&ctl.logs, &tx, generation, line, &mut None, &mut String::new());
            rx.try_iter().for_each(|m| ctl.handle(m));
        };
        let play = |ctl: &mut Ctl, from: f64, to: f64| {
            let mut m = from;
            while m < to && ctl.target.is_none() {
                ctl.enc.media = m;
                ctl.shared.q.lock().unwrap().written_pts = Some(BASE + ticks(m + 2.0));
                ctl.watchdog_work();
                m += 0.5;
            }
        };

        feed(&mut ctl, "[freezedetect @ 00000208bbfa5440] lavfi.freezedetect.freeze_start: 0.0100222");
        feed(&mut ctl, "[vf#0:0 @ 00000208bbfbe440] Reconfiguring filter graph because video parameters changed to yuv420p(tv, bt709), 1920x1080");
        play(&mut ctl, 20.0, 65.0);

        assert!(ctl.target.is_none(), "moving video judged frozen: {}", ctl.watch.last);
        assert!(!events(&ctl).iter().any(|(_, k)| k == "watchdog-freeze"), "{:?}", events(&ctl));

        // A real freeze after the rebuild is still caught once it has lasted d. The line is from the
        // offline run: slate, then this video, frozen from 45 s by the freeze-video feeder filter.
        feed(&mut ctl, "[freezedetect @ 0000018d23ae6c00] lavfi.freezedetect.freeze_start: 45.010022");
        play(&mut ctl, 65.0, 90.0);
        assert_eq!(ctl.enc.media, 65.5);
        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.reason)), Some((0, "retry")), "{}", ctl.watch.last);
        assert_eq!(ctl.watch.last["verdict"].as_str(), Some("freeze"));
    }

    /// (level, kind) of every event the test control loop logged.
    fn events(ctl: &Ctl) -> Vec<(String, String)> {
        let file = std::fs::read_dir(&ctl.logs.dir).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("events-"));
        file.map_or(String::new(), |f| std::fs::read_to_string(f.path()).unwrap()).lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).map(|v| (v["level"].as_str().unwrap().to_string(), v["kind"].as_str().unwrap().to_string())).collect()
    }

    #[test]
    fn a_failed_download_is_an_event_and_three_in_a_row_are_critical() {
        let mut ctl = test_ctl("dl-failed", &["a", "b", "c", "d"]);
        let fail = |ctl: &mut Ctl, index: usize, id: &str| {
            for _ in 0..2 {
                ctl.handle(Msg::Downloaded { index, id: id.into(), ok: false, cancelled: false, bytes: 0, ms: 10, reason: "ERROR: Postprocessing: ffmpeg not found".into() });
            }
        };

        fail(&mut ctl, 0, "a");
        fail(&mut ctl, 1, "b");
        fail(&mut ctl, 2, "c");
        fail(&mut ctl, 3, "d");

        let ev = events(&ctl);
        let count = |level: &str, kind: &str| ev.iter().filter(|(l, k)| l == level && k == kind).count();
        assert_eq!(count("warn", "download-failed"), 4, "{ev:?}");
        assert_eq!(count("critical", "downloads-failing"), 1, "{ev:?}");
    }

    #[test]
    fn a_feeder_that_will_not_spawn_logs_once_backs_off_and_keeps_its_target() {
        // test_ctl's root has no ffmpeg
        let mut ctl = test_ctl("spawn-failed", &["a", "b"]);

        for _ in 0..5 {
            ctl.start_feeder(1, Source::File, 30.0, "normal", false);
        }

        let critical = events(&ctl).iter().filter(|(l, k)| l == "critical" && k == "feeder-spawn-failed").count();
        assert_eq!(critical, 1, "one event per failure streak");
        assert!(ctl.feeder_after > Instant::now(), "the respawn is not backed off");
        assert_eq!(ctl.target.as_ref().map(|t| (t.index, t.pos, t.reason)), Some((1, 30.0, "normal")), "the video to feed was lost");
    }

    fn spare(index: usize, id: &str, expire: Option<u64>) -> Option<Spare> {
        let r = Resolved { video: "v".into(), audio: "a".into(), headers: String::new(), duration: None, bytes: None, expire, available_at: None };
        Some(Spare { index, id: id.into(), r, at: Instant::now() })
    }

    #[test]
    fn a_spare_arriving_inside_the_refresh_margin_is_not_resolved_again_before_the_interval() {
        let t0 = Instant::now();
        let mut due = Some((4, t0));
        let mut slot = None;

        store_spare(&mut slot, &mut due, spare(4, "abc", Some(2800)).unwrap(), t0);

        let ask = |after: f64| spare_wanted(due, 4, t0 + Duration::from_secs_f64(after), slot.as_ref(), "abc", 1000, None, 1800);
        assert!(!ask(5.0), "asked again at the next free slot");
        assert!(!ask(rules::SPARE_AFTER_S - 1.0));
        assert!(ask(rules::SPARE_AFTER_S + 1.0), "an expiring spare is replaced after the interval");
    }

    #[test]
    fn a_retry_that_used_the_spare_asks_for_the_next_one_at_once() {
        let t0 = Instant::now();
        let mut due = Some((4, t0));
        let mut slot = None;
        store_spare(&mut slot, &mut due, spare(4, "abc", Some(10_000)).unwrap(), t0);
        let t1 = t0 + Duration::from_secs(10);

        let used = retry_spare(&mut slot, &mut due, 4, "abc", 1000, 1800, None, t1);

        assert!(used.is_some());
        assert!(spare_wanted(due, 4, t1, slot.as_ref(), "abc", 1000, None, 1800), "no spare asked for after the retry used it");
    }

    #[test]
    fn a_retry_uses_the_spare_for_its_own_video() {
        let mut s = spare(4, "abc", Some(10_000));

        let taken = take_spare(&mut s, 4, "abc", 1000, 1800, None);

        assert!(taken.is_some());
        assert!(s.is_none(), "a used spare is gone");
    }

    #[test]
    fn a_retry_never_uses_another_videos_spare() {
        // Another index, and the same index after a playlist switch (another id).
        let mut other_index = spare(3, "abc", Some(10_000));
        let mut other_id = spare(4, "old", Some(10_000));

        assert!(take_spare(&mut other_index, 4, "abc", 1000, 1800, None).is_none());
        assert!(take_spare(&mut other_id, 4, "abc", 1000, 1800, None).is_none());
        assert!(other_index.is_none() && other_id.is_none(), "an unusable spare is dropped");
    }

    #[test]
    fn a_retry_never_uses_an_expired_or_expiring_spare() {
        let mut expired = spare(4, "abc", Some(900));
        let mut in_margin = spare(4, "abc", Some(2800));
        let mut unknown = spare(4, "abc", None);
        let mut ttl_expired = spare(4, "abc", Some(10_000));

        assert!(take_spare(&mut expired, 4, "abc", 1000, 1800, None).is_none());
        assert!(take_spare(&mut in_margin, 4, "abc", 1000, 1800, None).is_none());
        assert!(take_spare(&mut unknown, 4, "abc", 1000, 1800, None).is_none());
        assert!(take_spare(&mut ttl_expired, 4, "abc", 1000, 1800, Some(60)).is_none(), "the dev TTL wins over the URL's expiry");
    }

    fn only_log(dir: &std::path::Path, prefix: &str) -> String {
        let hits: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(prefix) && n.ends_with(".log")).collect();
        assert_eq!(hits.len(), 1, "{hits:?}");
        hits[0].clone()
    }

    #[test]
    fn encoder_log_keeps_the_stream_key_out() {
        let dir = std::env::temp_dir().join(format!("sl-redact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings { ingest_url: "rtmp://live.twitch.tv/app".into(), stream_key: "live_987654_SeCrEt".into(), ..Settings::default() };
        let logs = Logs::new(dir.clone(), &settings.stream_key);
        let line = format!("[flv @ 000001] {}: I/O error", config::target_url(&settings));

        encoder_log_line(&logs, 3, &line);

        let written = std::fs::read_to_string(dir.join(only_log(&dir, "encoder-"))).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(written.contains("rtmp://live.twitch.tv/app/***: I/O error"), "{written}");
        assert!(!written.contains("SeCrEt"), "{written}");
    }

    #[test]
    fn info_level_encoder_lines_are_logged_redacted_and_detector_lines_reach_the_loop() {
        let dir = std::env::temp_dir().join(format!("sl-redact-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings { ingest_url: "rtmp://live.twitch.tv/app".into(), stream_key: "live_987654_SeCrEt".into(), ..Settings::default() };
        let logs = Logs::new(dir.clone(), &settings.stream_key);
        let (tx, rx) = channel();
        let (mut out_us, mut speed) = (None, String::new());
        let lines = [
            format!("Output #0, flv, to '{}':", config::target_url(&settings)),
            "[freezedetect @ 0000029387615f40] lavfi.freezedetect.freeze_start: 12.5".to_string(),
            "out_time_us=2000000".to_string(),
            "progress=continue".to_string(),
        ];

        for l in &lines {
            encoder_stderr_line(&logs, &tx, 4, l, &mut out_us, &mut speed);
        }

        let written = std::fs::read_to_string(dir.join(only_log(&dir, "encoder-"))).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(written.contains("Output #0, flv, to 'rtmp://live.twitch.tv/app/***':"), "{written}");
        assert!(!written.contains("SeCrEt"), "{written}");
        assert!(written.contains("freeze_start: 12.5"));
        assert!(!written.contains("out_time_us"), "progress keys stay out of the log");
        let msgs: Vec<Msg> = rx.try_iter().collect();
        assert!(matches!(msgs[0], Msg::Detect { generation: 4, d: Detect::FreezeStart(t) } if t == 12.5));
        assert!(matches!(msgs[1], Msg::EncProgress { generation: 4, media, .. } if media == 2.0));
    }

    #[test]
    fn a_bad_key_on_reload_while_streaming_warns_once_and_stays_visible() {
        let unreadable = || Err(("stream-key-unreadable", "cannot decrypt".to_string()));
        assert_eq!(
            key_after_reload(false, false, unreadable()),
            KeyReload { problem: Some("cannot decrypt".into()), event: Some(("warn", "stream-key-unreadable")), start_encoder: false }
        );
        assert_eq!(key_after_reload(false, true, unreadable()).event, None, "one warning per streak");
        assert_eq!(key_after_reload(false, true, unreadable()).problem.as_deref(), Some("cannot decrypt"));
        assert_eq!(key_after_reload(false, true, Ok(())), KeyReload { problem: None, event: None, start_encoder: false });
        // needs-stream-key: the startup event was the one; a working key starts the encoder
        assert_eq!(key_after_reload(true, true, Err(("stream-key-missing", "none".into()))).event, None);
        assert_eq!(key_after_reload(true, true, Ok(())), KeyReload { problem: None, event: None, start_encoder: true });
    }

    #[test]
    fn status_shows_the_key_problem_and_what_it_waits_for() {
        let head = status_head(false, false, Some("cannot decrypt"), None, true, false);
        assert_eq!(head, json!({"state": "playing", "reason": null, "streamKeyProblem": "cannot decrypt"}));
        let head = status_head(false, true, Some("missing"), None, false, true);
        assert_eq!(head, json!({"state": "needs-stream-key", "reason": "missing", "streamKeyProblem": "missing"}));
        let head = status_head(false, false, None, Some("no such playlist"), false, false);
        assert_eq!(head, json!({"state": "waiting-for-playlist", "reason": "no such playlist", "streamKeyProblem": null}));
        assert_eq!(status_head(false, false, None, None, false, false)["state"], "starting");
        assert_eq!(status_head(false, false, None, None, true, true)["state"], "recovering");
        assert_eq!(status_head(true, true, Some("x"), None, true, false)["state"], "stopping");
    }

    #[test]
    fn a_failed_first_playlist_fetch_is_retried_and_reported_once() {
        assert_eq!(playlist_failure(true, false), PlaylistFailure { refetch: true, event: Some("critical") });
        assert_eq!(playlist_failure(true, true), PlaylistFailure { refetch: true, event: None });
        assert_eq!(playlist_failure(false, false), PlaylistFailure { refetch: false, event: Some("warn") });
    }

    #[test]
    fn a_duplicate_fetch_of_the_loaded_playlist_does_not_switch() {
        use PlaylistAction::*;
        // cold start: the first result loads (and resumes), a second one for the same id is dropped
        assert_eq!(playlist_action(true, "", "PLa", "PLa"), Load);
        assert_eq!(playlist_action(false, "PLa", "PLa", "PLa"), Drop);
        // a real switch, and a fetch a later reload superseded
        assert_eq!(playlist_action(false, "PLa", "PLb", "PLb"), Switch);
        assert_eq!(playlist_action(false, "PLa", "PLb", "PLa"), Drop);
        assert_eq!(playlist_action(true, "", "PLa", "PLb"), Drop);
    }

    #[test]
    fn test_flags_need_debug_faults_and_old_flags_are_gone() {
        let args = |a: &[&str]| parse_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(args(&["--root", "x", "--hours", "1"]).err().unwrap().contains("--debug-faults"));
        assert!(args(&["--root", "x", "--port", "1"]).err().unwrap().contains("--port"));
        let a = args(&["--root", "x", "--debug-faults", "--hours", "0.5", "--data", "d"]).unwrap();
        assert_eq!(a.dev.hours, Some(0.5));
        assert!(a.paths.data.ends_with("d") && a.config.ends_with("app/config.json"));
    }
}
