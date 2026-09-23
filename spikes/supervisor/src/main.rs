//! Spike 5 supervisor (Notion: "Spike 5: supervisor (Rust)"). The control loop owns all state;
//! the API, downloader, resolves, pumps and encoder progress reach it over one channel.

mod api;
mod media;
mod procs;
mod rules;
mod ytdlp;

use media::{Input, Live, Queue, Shared};
use rules::{secs, ticks, Ladder, Seam, Source, BASE, FRAME};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ytdlp::{Entry, Resolved, Ytdlp};


#[derive(Clone)]
pub struct Logs {
    dir: PathBuf,
}

impl Logs {
    pub fn append(&self, file: &str, text: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join(file)) {
            let _ = f.write_all(text.as_bytes());
        }
    }
    pub fn csv(&self, file: &str, row: &[String]) {
        let clean: Vec<String> = row.iter().map(|c| c.replace([',', '\r', '\n'], " ")).collect();
        self.append(file, &format!("{}\n", clean.join(",")));
    }
    pub fn event(&self, level: &str, kind: &str, extra: Value) {
        let mut v = json!({"ts": iso_now(), "level": level, "kind": kind});
        if let (Some(m), Value::Object(e)) = (v.as_object_mut(), extra) {
            m.extend(e);
        }
        eprintln!("{level:8} {kind} {}", v);
        self.append("events.jsonl", &format!("{v}\n"));
    }
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


pub struct Args {
    playlist: String,
    start: usize,
    max_videos: usize,
    budget: u64,
    limit_rate: Option<String>,
    hours: f64,
    target: String,
    codec: String,
    read_ahead: f64,
    buffer_mb: usize,
    port: u16,
    debug_faults: bool,
    url_ttl: Option<u64>,
    refresh_margin: u64,
    state_file: PathBuf,
    resume: bool,
    token_server: String,
    slate: String,
    ytdlp: String,
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| argv.iter().any(|a| a == &format!("--{name}"));
    let opt = |name: &str| argv.iter().position(|a| a == &format!("--{name}")).and_then(|i| argv.get(i + 1)).cloned();
    let num = |name: &str, d: f64| -> Result<f64, String> {
        opt(name).map(|v| v.parse::<f64>().map_err(|_| format!("--{name} {v}: not a number"))).unwrap_or(Ok(d))
    };
    let abs = |p: String| std::env::current_dir().unwrap().join(p).to_string_lossy().into_owned();
    Ok(Args {
        playlist: opt("playlist").unwrap_or("https://www.youtube.com/playlist?list=PL6B3937A5D230E335".into()),
        start: num("start", 0.0)? as usize,
        max_videos: num("max-videos", 0.0)? as usize,
        budget: (num("budget-mb", 300.0)? * 1048576.0) as u64,
        limit_rate: opt("limit-rate"),
        hours: num("hours", 1.0)?,
        target: opt("target").unwrap_or("rtmp://127.0.0.1:1935/live/spike".into()),
        codec: opt("codec").unwrap_or("h264_nvenc".into()),
        read_ahead: num("read-ahead-seconds", 20.0)?,
        buffer_mb: num("buffer-mb", 192.0)? as usize,
        port: num("port", 4417.0)? as u16,
        debug_faults: flag("debug-faults"),
        url_ttl: opt("url-ttl-s").map(|v| v.parse().map_err(|_| "--url-ttl-s: not a number".to_string())).transpose()?,
        refresh_margin: num("url-refresh-margin-s", 1800.0)? as u64,
        state_file: PathBuf::from(abs(opt("state-file").unwrap_or("state.json".into()))),
        resume: flag("resume"),
        token_server: opt("token-server").unwrap_or("C:/Users/anton/bgutil-ytdlp-pot-provider/server/build/main.js".into()),
        slate: abs(opt("slate").unwrap_or("../buffer/media/slate.mp4".into())),
        ytdlp: abs(opt("ytdlp").unwrap_or("../../yt-dlp/yt-dlp.exe".into())),
    })
}


#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Purpose {
    Play,
    Refresh,
    Size,
}

pub enum Msg {
    Status(Sender<Value>),
    Control { cmd: String, index: Option<i64>, reply: Sender<Result<Value, String>> },
    Fault { kind: String, count: Option<u32>, reply: Sender<Result<Value, String>> },
    Shutdown,
    Playlist(Result<Vec<Entry>, String>),
    Resolved { index: usize, purpose: Purpose, result: Result<Resolved, String> },
    FeederExit { seq: u64, code: Option<i32>, frames: u64 },
    EncProgress { generation: u64, media: f64 },
    DlProgress { index: usize, percent: f64 },
    Downloaded { index: usize, ok: bool, cancelled: bool, bytes: u64, ms: u128, reason: String },
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
}

pub struct TokenState {
    state: &'static str,
    restarts: u32,
    kill: bool,
    stop: bool,
}

struct Ctl {
    args: Args,
    logs: Logs,
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    yt: Arc<Ytdlp>,
    cache: PathBuf,
    videos: Vec<Video>,
    seams: Vec<Seam>,
    enc: Encoder,
    live: Option<Feeder>,
    metas: HashMap<u64, FeederMeta>,
    feeder_seq: u64,
    target: Option<Target>,
    resolved: HashMap<usize, (Resolved, Instant)>,
    resolving: HashSet<(usize, bool)>,
    refresh_after: Instant,
    refresh_pending: Option<usize>,
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
    control_seq: u64,
    ever_played: bool,
    stopping: bool,
    started_at: u128,
    deadline: Instant,
    cache_bytes: u64,
    max_cache: u64,
    seam_rows: u64,
    counts: HashMap<&'static str, u64>,
    last_state: Instant,
    last_buffer: Instant,
    last_scan: Instant,
}

const STALL: Duration = Duration::from_secs(10);
const FILLER_BELOW_S: f64 = 2.0;

impl Ctl {
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
            Some((s, pos)) => (s.index, pos, s.source.clone()),
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


    fn start_feeder(&mut self, index: usize, source: Source, pos: f64, reason: &'static str, filler: bool) {
        let file = self.file_of(index);
        let (resolved, expire) = match source {
            Source::Url => {
                let (r, at) = self.resolved.get(&index).cloned().expect("url feeder without a resolve");
                let ttl_expire = self.args.url_ttl.map(|t| unix_ms() as u64 / 1000 - at.elapsed().as_secs() + t);
                let e = ttl_expire.or(r.expire);
                (Some(r), e)
            }
            _ => (None, None),
        };
        let path = match source {
            Source::Slate => self.args.slate.clone(),
            _ => file.to_string_lossy().into_owned(),
        };
        let input = match &resolved {
            Some(r) => Input::Url { video: &r.video, audio: &r.audio, headers: &r.headers },
            None => Input::File(&path),
        };
        self.feeder_seq += 1;
        self.refresh_pending = None;
        let seq = self.feeder_seq;
        if !filler {
            self.filler_since = None;
        } else if self.filler_since.is_none() {
            self.filler_since = Some(seq);
        }
        let offset = self.queue_end();
        let spawned = procs::command("ffmpeg")
            .args(media::feeder_args(&input, rules::seek_for(&source, pos), offset))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                self.logs.event("critical", "feeder-spawn-failed", json!({"error": e.to_string()}));
                return;
            }
        };
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
                    logs.append("feeders.log", &format!("{} {tag} {t}\n", iso_now()));
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
        self.logs.event("info", "feeder-start", json!({"seq": seq, "name": name, "index": index, "source": source.name(), "pos": pos, "offset": secs(offset), "reason": reason}));
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

    /// Drop the slate that is only covering a wait: every filler since the last real feeder.
    fn cut_filler(&mut self) {
        if self.live.as_ref().is_some_and(|f| f.filler) {
            self.cut_live();
        }
        if let Some(seq) = self.filler_since.take() {
            self.shared.q.lock().unwrap().flush_from(seq);
        }
    }

    fn next_playable(&mut self, from: usize) -> usize {
        let n = self.n();
        if let Some(i) = (0..n).map(|d| (from + d) % n).find(|&i| !self.videos[i].skipped) {
            return i;
        }
        self.logs.event("critical", "all-skipped", json!({}));
        self.videos.iter_mut().for_each(|v| v.skipped = false);
        from % n
    }

    fn set_target(&mut self, index: usize, pos: f64, reason: &'static str, gated: bool) {
        let (index, pos) = rules::resolve_start(index, pos, self.duration(index), self.n().max(1));
        // A fresh video gets a fresh retry budget; retries, refreshes and restarts keep theirs.
        if matches!(reason, "normal" | "skip" | "control" | "coldstart") {
            if let Some(v) = self.videos.get_mut(index) {
                v.retries = 0;
            }
        }
        self.target = Some(Target { index, pos, reason, gated });
    }

    /// Ladder levels 1 and 2 for the video a feeder or resolve failed on.
    fn fail(&mut self, index: usize, fed: f64, why: &str, detail: Value) {
        let retries = self.videos[index].retries;
        match rules::after_failure(retries) {
            Ladder::Retry => {
                self.videos[index].retries += 1;
                self.resolved.remove(&index);
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

    fn request_resolve(&mut self, index: usize, purpose: Purpose) {
        let key = (index, purpose == Purpose::Size);
        if !self.resolving.insert(key) {
            return;
        }
        let (yt, logs, tx, id) = (self.yt.clone(), self.logs.clone(), self.tx.clone(), self.id(index));
        std::thread::spawn(move || {
            let result = yt.resolve(&id, &logs);
            let _ = tx.send(Msg::Resolved { index, purpose, result });
        });
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
        let mut child = match procs::command("ffmpeg")
            .args(media::encoder_args(&self.args.target, &self.args.codec))
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
                let t = line.trim();
                match t.split_once('=') {
                    Some(("out_time_us", v)) => out_us = Some(v.to_string()),
                    Some(("speed", v)) => speed = v.trim().to_string(),
                    Some(("progress", _)) => {
                        if let Some(us) = out_us.as_ref().and_then(|u| u.parse::<i64>().ok()) {
                            logs.csv("encoder.csv", &[unix_ms().to_string(), us.to_string(), speed.clone(), generation.to_string()]);
                            let _ = tx.send(Msg::EncProgress { generation, media: us as f64 / 1e6 });
                        }
                    }
                    Some((k, _)) if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {}
                    _ if !t.is_empty() => logs.append("encoder.log", &format!("{} [gen {generation}] {t}\n", iso_now())),
                    _ => {}
                }
            }
        });
        if self.counts.contains_key("encoderStarts") {
            self.enc.restarts += 1;
        }
        self.count("encoderStarts");
        self.enc.child = Some(child);
        self.enc.media = 0.0;
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
        self.enc.failures += 1;
        let wait = rules::encoder_backoff_s(self.enc.failures - 1);
        self.enc.restart_at = Some(Instant::now() + Duration::from_secs(wait));
        self.enc.ingest_was_down = false;
        self.step = "encoder-restart";
        self.step_offset = BASE;
        self.count("encoderExits");
        let level = if self.enc.failures >= 3 { "critical" } else { "warn" };
        self.logs.event(level, "encoder-exit", json!({"why": why, "code": code, "failures": self.enc.failures, "restartInS": wait, "resumeIndex": index, "resumePos": pos}));
        if !self.videos.is_empty() && !self.stopping {
            self.set_target(index, pos, "encoder-restart", false);
        }
    }

    /// Is the RTMP ingest accepting connections? Only probed while the encoder is down.
    fn ingest_up(&self) -> bool {
        let hostport = self.args.target.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
        use std::net::ToSocketAddrs;
        let addr = hostport.to_socket_addrs().ok().and_then(|mut a| a.next());
        addr.is_some_and(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
    }


    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.deadline && !self.stopping {
            self.logs.event("info", "deadline", json!({"hours": self.args.hours}));
            self.shutdown();
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
                if expire.is_some_and(|e| rules::refresh_due(unix, e, self.args.refresh_margin)) && !self.resolving.contains(&(index, false)) {
                    self.logs.event("info", "url-refresh", json!({"index": index, "id": self.id(index), "expire": expire, "now": unix}));
                    self.refresh_after = now + Duration::from_secs(30);
                    self.request_resolve(index, Purpose::Refresh);
                }
            }
            if let Some(f) = &self.live {
                if !f.filler && f.started.elapsed() > Duration::from_secs(300) {
                    let i = f.index;
                    self.videos[i].retries = 0;
                }
            }
        }

        // The target: what should be fed next.
        if let Some(t) = self.target.clone() {
            if self.live.as_ref().is_none_or(|f| f.filler) && !self.videos.is_empty() {
                let on_disk = self.videos[t.index].on_disk;
                if !on_disk && !self.fresh_url(t.index) {
                    self.request_resolve(t.index, Purpose::Play);
                }
                let gate_open = !t.gated || self.read_ahead() < self.args.read_ahead;
                if gate_open && (on_disk || self.url_ready(t.index)) {
                    self.cut_filler();
                    self.target = None;
                    let source = if on_disk { Source::File } else { Source::Url };
                    self.start_feeder(t.index, source, t.pos, t.reason, false);
                }
            }
        }

        // Nothing to feed and the pipe running dry: the slate covers it.
        if self.live.is_none() && self.read_ahead() < FILLER_BELOW_S {
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
        self.cache_work();
        self.periodic();
    }


    fn window(&self) -> Vec<usize> {
        let sizes: Vec<Option<u64>> = self.videos.iter().map(|v| if v.on_disk { Some(v.disk_bytes) } else { v.bytes }).collect();
        let skipped: Vec<bool> = self.videos.iter().map(|v| v.skipped || v.dl_failed).collect();
        rules::window(self.view().0, &sizes, &skipped, self.args.budget)
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
                self.logs.csv("deletes.csv", &[iso_now(), self.id(i), bytes.to_string(), format!("{:.6}", secs(self.view_timeline())), end]);
            }
        }

        // "prev" lands on a video that playout already deleted: keep its URL resolved and past
        // available_at, so the cut does not wait for yt-dlp plus the site's delay.
        let prev = (view + self.n() - 1) % self.n();
        if !self.videos[prev].on_disk && !self.fresh_url(prev) && !self.videos[prev].skipped {
            self.request_resolve(prev, Purpose::Play);
        }

        // Dispatch one job when the downloader is idle.
        if self.downloading.is_some() || self.sizing || Instant::now() < self.dl_after {
            return;
        }
        let on_disk: Vec<bool> = self.videos.iter().map(|v| v.on_disk).collect();
        let failed: Vec<bool> = self.videos.iter().map(|v| v.dl_failed || v.skipped).collect();
        let pick = first.filter(|&u| !failed[u]).or_else(|| rules::next_download(&win, &on_disk, &failed));
        let Some(i) = pick else { return };
        if self.videos[i].bytes.is_none() && i != view && Some(i) != first {
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
            let tmp = self.args.state_file.with_extension("tmp");
            if std::fs::write(&tmp, v.to_string()).is_ok() {
                let _ = std::fs::rename(&tmp, &self.args.state_file);
            }
        }
        if self.last_buffer.elapsed() >= Duration::from_secs(10) {
            self.last_buffer = Instant::now();
            let playing = if source == Source::Slate { "slate".into() } else { self.id(index) };
            let (dl_id, pct) = self.downloading.as_ref().map_or((String::new(), String::new()), |d| (self.id(d.0), format!("{:.1}", d.1)));
            self.logs.csv(
                "buffer.csv",
                &[iso_now(), self.cache_bytes.to_string(), self.videos.iter().filter(|v| v.on_disk).count().to_string(), dl_id, pct, playing, format!("{:.1}", self.enc.media), format!("{:.2}", self.read_ahead())],
            );
            self.save_run();
        }
    }


    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Msg::Control { cmd, index, reply } => {
                let _ = reply.send(self.control(&cmd, index));
            }
            Msg::Fault { kind, count, reply } => {
                let _ = reply.send(self.fault(&kind, count));
            }
            Msg::Shutdown => self.shutdown(),
            Msg::Playlist(Err(e)) => {
                self.logs.event("critical", "playlist-failed", json!({"reason": e}));
                self.shutdown_code(2);
            }
            Msg::Playlist(Ok(mut entries)) => {
                entries.drain(..self.args.start.min(entries.len()));
                if self.args.max_videos > 0 {
                    entries.truncate(self.args.max_videos);
                }
                if entries.is_empty() {
                    self.logs.event("critical", "playlist-failed", json!({"reason": "no entries after --start/--max-videos"}));
                    return self.shutdown_code(2);
                }
                self.logs.append("playlist.json", &serde_json::to_string_pretty(&entries.iter().map(|e| json!({"id": e.id, "title": e.title, "duration": e.duration})).collect::<Vec<_>>()).unwrap());
                self.videos = entries.into_iter().map(|entry| Video { entry, bytes: None, on_disk: false, disk_bytes: 0, dl_attempts: 0, dl_failed: false, skipped: false, retries: 0 }).collect();
                self.logs.event("info", "playlist", json!({"count": self.n()}));
                let resume = self.args.resume.then(|| std::fs::read_to_string(&self.args.state_file).ok()).flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok());
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
            Msg::Resolved { index, purpose, result } => self.on_resolved(index, purpose, result),
            Msg::FeederExit { seq, code, frames } => self.on_feeder_exit(seq, code, frames),
            Msg::EncProgress { generation, media } => {
                let mut q = self.shared.q.lock().unwrap();
                if generation == q.generation && self.enc.child.is_some() {
                    if media > self.enc.media {
                        self.enc.last_progress = Instant::now();
                    }
                    self.enc.media = media;
                    q.view = BASE + ticks(media);
                    self.shared.cv.notify_all();
                }
            }
            Msg::DlProgress { index, percent } => {
                if let Some(d) = self.downloading.as_mut().filter(|d| d.0 == index) {
                    d.1 = percent;
                }
            }
            Msg::Downloaded { index, ok, cancelled, bytes, ms, reason } => {
                self.downloading = None;
                let v = &mut self.videos[index];
                let result = if cancelled {
                    "cancel"
                } else if ok {
                    v.on_disk = true;
                    v.disk_bytes = bytes;
                    v.bytes = Some(bytes);
                    v.dl_attempts = 0;
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
                let attempt = self.videos[index].dl_attempts;
                self.logs.csv("downloads.csv", &[iso_now(), self.id(index), result.into(), attempt.to_string(), bytes.to_string(), ms.to_string(), reason]);
            }
        }
    }

    fn on_resolved(&mut self, index: usize, purpose: Purpose, result: Result<Resolved, String>) {
        self.resolving.remove(&(index, purpose == Purpose::Size));
        if purpose == Purpose::Size {
            self.sizing = false;
        }
        let mut r = match result {
            Ok(r) => r,
            Err(reason) => {
                self.logs.event("warn", "resolve-failed", json!({"index": index, "id": self.id(index), "purpose": format!("{purpose:?}"), "reason": reason}));
                match purpose {
                    Purpose::Size => {
                        self.videos[index].dl_failed = true;
                        self.logs.csv("downloads.csv", &[iso_now(), self.id(index), "skip".into(), "1".into(), "0".into(), "0".into(), reason]);
                    }
                    Purpose::Play => {
                        if let Some(t) = self.target.clone().filter(|t| t.index == index) {
                            self.fail(index, t.pos, "resolve", json!({"reason": reason}));
                        }
                    }
                    Purpose::Refresh => {}
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
        self.resolved.insert(index, (r, Instant::now()));
        if purpose == Purpose::Refresh {
            self.refresh_pending = Some(index);
        }
    }

    fn on_feeder_exit(&mut self, seq: u64, code: Option<i32>, frames: u64) {
        if let Some(m) = self.metas.remove(&seq) {
            self.seam_rows += 1;
            self.logs.csv(
                "seams.csv",
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
            if self.target.is_none() {
                let next = self.next_playable(index + 1);
                self.set_target(next, 0.0, "normal", true);
            }
        }
    }


    fn status(&self) -> Value {
        let (index, pos, source) = self.view();
        let state = if self.stopping {
            "stopping"
        } else if !self.ever_played {
            "starting"
        } else if self.enc.child.is_none() || self.step != "none" {
            "recovering"
        } else {
            "playing"
        };
        let v = self.videos.get(index);
        // One lock per statement: json! keeps its temporaries (and their guards) alive to the end.
        let token = { let t = self.token.lock().unwrap(); json!({"state": t.state, "restarts": t.restarts}) };
        let generation = self.shared.q.lock().unwrap().generation;
        json!({
            "state": state,
            "playing": {"index": index, "id": v.map(|v| v.entry.id.clone()), "title": v.map(|v| v.entry.title.clone()), "source": source.name(), "position": (pos * 1000.0).round() / 1000.0, "duration": v.and_then(|v| v.entry.duration)},
            "timeline": secs(self.view_timeline()),
            "readAheadSeconds": (self.read_ahead() * 100.0).round() / 100.0,
            "cacheBytes": self.cache_bytes,
            "downloading": self.downloading.as_ref().map(|d| json!({"index": d.0, "percent": d.1})),
            "encoder": {"alive": self.enc.child.is_some(), "restarts": self.enc.restarts, "generation": generation, "mediaTime": self.enc.media},
            "tokenServer": token,
            "recovery": {"step": self.step, "retries": v.map_or(0, |v| v.retries)},
            "playlistCount": self.n(),
        })
    }

    fn control(&mut self, cmd: &str, index: Option<i64>) -> Result<Value, String> {
        if self.stopping {
            return Err("stopping".into());
        }
        let view = self.view().0;
        let t = rules::control_target(cmd, index, view, self.n())?;
        self.cut_live();
        // The kept second is on air whatever it is; a later cut_filler only drops a new slate.
        self.filler_since = None;
        let flushed = self.shared.q.lock().unwrap().flush_keeping(rules::HZ);
        let cut = self.queue_end();
        self.control_seq += 1;
        self.count("controls");
        self.videos[t].skipped = false;
        self.videos[t].retries = 0;
        self.step = "none";
        self.set_target(t, 0.0, "control", false);
        self.logs.csv("controls.csv", &[unix_ms().to_string(), cmd.into(), index.map_or(String::new(), |i| i.to_string()), t.to_string(), self.id(t), format!("{:.6}", secs(cut)), flushed.to_string()]);
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
            }
            "kill-encoder" => {
                procs::kill_tree(self.enc.child.as_ref().ok_or("no encoder")?.id());
            }
            "kill-token-server" => self.token.lock().unwrap().kill = true,
            _ => return Err(format!("unknown fault {kind:?}")),
        }
        Ok(json!({"accepted": true, "kind": kind}))
    }

    fn save_run(&self) {
        let a = &self.args;
        let token = { let t = self.token.lock().unwrap(); json!({"state": t.state, "restarts": t.restarts}) };
        let v = json!({
            "startedAt": self.started_at, "endedAt": if self.stopping { Some(unix_ms()) } else { None },
            "target": a.target, "codec": a.codec, "hours": a.hours, "baseOffset": secs(BASE), "bufferMb": a.buffer_mb,
            "readAheadSeconds": a.read_ahead, "playlist": a.playlist, "budgetBytes": a.budget, "limitRate": a.limit_rate,
            "start": a.start, "maxVideos": a.max_videos, "urlTtlS": a.url_ttl, "urlRefreshMarginS": a.refresh_margin,
            "resume": a.resume, "debugFaults": a.debug_faults, "entries": self.n(),
            "seams": self.seam_rows, "encoderExitedEarly": self.counts.contains_key("encoderExits"),
            "maxCacheBytes": self.max_cache, "counts": self.counts,
            "skipped": self.videos.iter().filter(|v| v.skipped).map(|v| v.entry.id.clone()).collect::<Vec<_>>(),
            "tokenServer": token,
        });
        let _ = std::fs::write(self.logs.dir.join("run.json"), serde_json::to_string_pretty(&v).unwrap());
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
                let _ = tx.send(Msg::Resolved { index, purpose: Purpose::Size, result });
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
                let _ = tx.send(Msg::Downloaded { index, ok, cancelled: out.cancelled, bytes, ms: started.elapsed().as_millis(), reason });
            }
        }
    }
}

const TOKEN_PORT: u16 = 4416;

/// Adopt a running PO-token server, or own one; replace it when it dies or hangs.
fn token_watchdog(state: Arc<Mutex<TokenState>>, script: String, logs: Logs) {
    let mut owned: Option<Child> = None;
    let spawn = |logs: &Logs| match ytdlp::spawn_token_server(&script, TOKEN_PORT, logs) {
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
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = procs::enter_job() {
        eprintln!("warning: {e}; children will not die with the supervisor");
    }
    let cwd = std::env::current_dir().unwrap();
    let logs = Logs { dir: cwd.join("logs") };
    let _ = std::fs::create_dir_all(&logs.dir);
    let cache = cwd.join("cache");
    if !args.resume {
        let _ = std::fs::remove_dir_all(&cache);
    }
    let _ = std::fs::create_dir_all(&cache);

    let deno = Path::new(&args.ytdlp).with_file_name("deno.exe");
    let yt = Arc::new(Ytdlp { exe: args.ytdlp.clone(), deno: deno.exists().then(|| deno.to_string_lossy().into_owned()), limit_rate: args.limit_rate.clone() });
    let (tx, rx) = channel::<Msg>();
    let mut queue = Queue::new(args.buffer_mb.max(1) * 1024 * 1024);
    queue.ahead = ticks(args.read_ahead);
    let shared = Arc::new(Shared { q: Mutex::new(queue), cv: Condvar::new() });
    let token = Arc::new(Mutex::new(TokenState { state: "down", restarts: 0, kill: false, stop: false }));
    let (jobs_tx, jobs_rx) = channel::<Job>();

    {
        let s = shared.clone();
        std::thread::spawn(move || media::writer(&s));
    }
    {
        let (t, script, l) = (token.clone(), args.token_server.clone(), logs.clone());
        std::thread::spawn(move || token_watchdog(t, script, l));
    }
    {
        let (yt, l, tx, c) = (yt.clone(), logs.clone(), tx.clone(), cache.clone());
        std::thread::spawn(move || downloader(jobs_rx, yt, l, tx, c));
    }
    {
        let (yt, l, tx, url) = (yt.clone(), logs.clone(), tx.clone(), args.playlist.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Playlist(yt.playlist(&url, &l)));
        });
    }
    if let Err(e) = api::serve(args.port, args.debug_faults, tx.clone()) {
        logs.event("critical", "api-failed", json!({"error": e}));
        std::process::exit(2);
    }

    let now = Instant::now();
    let mut ctl = Ctl {
        deadline: now + Duration::from_secs_f64(args.hours * 3600.0),
        args,
        logs,
        tx,
        shared,
        yt,
        cache,
        videos: vec![],
        seams: vec![],
        enc: Encoder { child: None, media: 0.0, last_progress: now, started: now, restarts: 0, failures: 0, restart_at: None, ingest_was_down: false },
        live: None,
        metas: HashMap::new(),
        feeder_seq: 0,
        target: None,
        resolved: HashMap::new(),
        resolving: HashSet::new(),
        refresh_after: now,
        refresh_pending: None,
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
    };
    ctl.logs.event("info", "start", json!({"target": ctl.args.target, "codec": ctl.args.codec, "resume": ctl.args.resume, "deno": ctl.yt.deno}));
    ctl.save_run();
    ctl.start_encoder();
    let mut last_tick = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(msg) => ctl.handle(msg),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => unreachable!("the control loop holds a sender"),
        }
        if last_tick.elapsed() >= Duration::from_millis(100) {
            last_tick = Instant::now();
            ctl.tick();
        }
    }
}
