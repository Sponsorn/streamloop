//! The supervisor's events file as the control process reads it: the 2 s tail, the mapping from an event to
//! timeline text and an optional Discord alert, and the once-per-outage gate on those alerts.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;
use serde_json::{Map, Value};
use streamloop_common::config::Playlist;
use streamloop_common::events::events_file;
use streamloop_common::time::{iso, local_date};

use crate::clock::Clock;
use crate::setup_check::{FFMPEG_MISSING, NO_ENCODER, OBS_RUNNING};

/// One line of the supervisor's events file, or an event the launcher raised in the same shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SupervisorEvent {
    pub ts: String,
    /// `info`, `warn` or `critical`; kept as text because the file is not checked for it.
    pub level: String,
    pub kind: String,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl SupervisorEvent {
    /// `None` unless the value is an object with a string `kind`, the only field the tail insists on.
    pub fn from_value(v: Value) -> Option<SupervisorEvent> {
        match v {
            Value::Object(m) => Self::from_map(m),
            _ => None,
        }
    }

    pub fn from_map(mut m: Map<String, Value>) -> Option<SupervisorEvent> {
        let kind = match m.remove("kind") {
            Some(Value::String(k)) => k,
            _ => return None,
        };
        let mut text = |key: &str| match m.remove(key) {
            Some(Value::String(s)) => s,
            _ => String::new(),
        };
        let ts = text("ts");
        let level = text("level");
        Some(SupervisorEvent { ts, level, kind, rest: m })
    }
}

/// The ten toggles under `discord.events` in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordKind {
    Error,
    Skip,
    Recovery,
    Critical,
    Resume,
    StreamDrop,
    StreamRestart,
    OutputFrozen,
    TwitchMismatch,
    TwitchRestart,
}

impl DiscordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DiscordKind::Error => "error",
            DiscordKind::Skip => "skip",
            DiscordKind::Recovery => "recovery",
            DiscordKind::Critical => "critical",
            DiscordKind::Resume => "resume",
            DiscordKind::StreamDrop => "streamDrop",
            DiscordKind::StreamRestart => "streamRestart",
            DiscordKind::OutputFrozen => "outputFrozen",
            DiscordKind::TwitchMismatch => "twitchMismatch",
            DiscordKind::TwitchRestart => "twitchRestart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Info,
    Warn,
    Error,
}

impl AlertLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertLevel::Info => "info",
            AlertLevel::Warn => "warn",
            AlertLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub kind: DiscordKind,
    /// Template variables in the order Node builds them; each value is a string or a number.
    pub vars: Vec<(String, Value)>,
    pub level: AlertLevel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    /// Timeline text. Empty means the event is too frequent for the 100-entry timeline: skip it.
    pub message: String,
    pub discord: Option<Alert>,
}

const DETAIL_MAX: usize = 200;

/// Twitch cuts a broadcast at 48 h; an encoder that ran this long and then dropped hit that limit.
const TWITCH_LIMIT_S: f64 = 47.5 * 3600.0;

/// One per video, seam or probe: they would push everything else out of the 100-entry timeline.
const QUIET: [&str; 6] = ["feeder-start", "handover", "spare-resolve", "spare-used", "youtube-probe", "download-cancel"];

/// A number as JavaScript prints it, so messages and variables read the same as before the port.
fn js_num(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn num_value(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9e15 {
        Value::from(f as i64)
    } else {
        serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

/// `JSON.stringify` for a value that is not a string.
fn stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), js_num),
        Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        Value::Array(a) => format!("[{}]", a.iter().map(stringify).collect::<Vec<_>>().join(",")),
        Value::Object(o) => {
            let items: Vec<String> = o.iter().map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap_or_default(), stringify(v))).collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

/// One line, at most 200 characters: safe to put in a Discord message.
fn one(v: Option<&Value>) -> String {
    let s = match v {
        None | Some(Value::Null) => return String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => stringify(other),
    };
    let first = s.split('\n').next().unwrap_or("");
    let line = first.strip_suffix('\r').unwrap_or(first).trim();
    let units: Vec<u16> = line.encode_utf16().collect();
    if units.len() > DETAIL_MAX {
        format!("{}...", String::from_utf16_lossy(&units[..DETAIL_MAX - 3]))
    } else {
        line.to_string()
    }
}

fn get<'a>(e: &'a SupervisorEvent, key: &str) -> Option<&'a Value> {
    e.rest.get(key)
}

fn num(e: &SupervisorEvent, key: &str) -> f64 {
    get(e, key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The supervisor counts videos from 0; people count from 1.
fn video(e: &SupervisorEvent) -> f64 {
    num(e, "index") + 1.0
}

/// A list as people know it: the name `with_playlist_names` found, else the id under `key`.
fn list_name(e: &SupervisorEvent, key: &str) -> String {
    let name = one(get(e, "name"));
    if name.is_empty() {
        one(get(e, key))
    } else {
        name
    }
}

/// The lists an event says could not load, ready for a sentence; empty when there are none.
fn missing_lists(e: &SupervisorEvent) -> String {
    match get(e, "missing") {
        Some(Value::Array(a)) => a.iter().map(|v| one(Some(v))).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

fn plain(message: impl Into<String>) -> Described {
    Described { message: message.into(), discord: None }
}

fn alert(message: impl Into<String>, kind: DiscordKind, vars: Vec<(&str, Value)>, level: AlertLevel) -> Described {
    let alert = Alert { kind, vars: vars.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), level };
    Described { message: message.into(), discord: Some(alert) }
}

fn critical_with(message: String, timeline: String) -> Described {
    alert(timeline, DiscordKind::Critical, vec![("message", Value::String(message))], AlertLevel::Error)
}

fn critical(message: String) -> Described {
    critical_with(message.clone(), message)
}

/// Through the critical toggle at warning level: the only template that carries free text.
fn warning(message: String) -> Described {
    alert(message.clone(), DiscordKind::Critical, vec![("message", Value::String(message))], AlertLevel::Warn)
}

/// The supervisor knows lists by id; this adds the names people gave them in the config.
pub fn with_playlist_names(mut e: SupervisorEvent, playlists: &[Playlist]) -> SupervisorEvent {
    if !matches!(e.kind.as_str(), "playlist-advance" | "playlist-restart" | "playlist-failed") {
        return e;
    }
    let name_of = |id: Option<&Value>| -> Option<String> {
        let id = id?.as_str()?;
        playlists.iter().find(|p| p.id == id)?.name.clone()
    };
    let name = name_of(get(&e, if e.kind == "playlist-advance" { "to" } else { "playlistId" }));
    if let Some(Value::Array(missing)) = get(&e, "missing") {
        let named: Vec<Value> = missing.iter().map(|id| name_of(Some(id)).map_or_else(|| id.clone(), Value::String)).collect();
        e.rest.insert("missing".into(), Value::Array(named));
    }
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        e.rest.insert("name".into(), Value::String(name));
    }
    e
}

/// The one mapping from a supervisor or launcher event to timeline text and an optional Discord alert.
pub fn describe(e: &SupervisorEvent) -> Described {
    use DiscordKind as K;
    if QUIET.contains(&e.kind.as_str()) {
        return plain("");
    }
    let n = video(e);
    let n_text = js_num(n);
    let is_critical = e.level == "critical";
    match e.kind.as_str() {
        "encoder-exit" => {
            let failures = num(e, "failures");
            if failures == 1.0 && num(e, "uptimeS") >= TWITCH_LIMIT_S {
                return plain(format!("Twitch ended the stream at its 48 hour limit; restarting in {}s", js_num(num(e, "restartInS"))));
            }
            let msg = format!("Encoder stopped ({}); restarting in {}s", one(get(e, "why")), js_num(num(e, "restartInS")));
            if is_critical {
                return critical_with(format!("The encoder has stopped {} times in a row ({}).", js_num(failures), one(get(e, "why"))), msg);
            }
            // 3 failures in a row is where the supervisor goes critical, so it is the limit shown here.
            alert(msg, K::StreamDrop, vec![("attempt", num_value(failures)), ("maxAttempts", 3.into())], AlertLevel::Warn)
        }
        "encoder-start" => {
            let restarts = num(e, "restarts");
            if restarts == 0.0 {
                return plain("Encoder started");
            }
            alert(format!("Encoder restarted (restart {})", js_num(restarts)), K::StreamRestart, vec![("attempts", num_value(restarts))], AlertLevel::Info)
        }
        "encoder-spawn-failed" => critical(format!("The encoder could not start: {}", one(get(e, "error")))),
        "skip" => {
            // detail is a string, or an object like {reason} or {since, pos}: only text reaches the message.
            let detail = get(e, "detail");
            let d = match detail {
                Some(Value::Object(o)) => o.get("reason"),
                Some(Value::Array(_)) => None,
                other => other,
            };
            let why = one(get(e, "why"));
            let reason = match d {
                Some(Value::String(s)) if !s.trim().is_empty() => one(Some(&Value::String(format!("{why}: {}", one(d))))),
                _ => one(Some(&Value::String(why))),
            };
            let msg = format!("Skipped video {n_text} ({reason})");
            if is_critical {
                return critical_with(format!("{} videos in a row were skipped. The last was video {n_text} ({reason}).", js_num(num(e, "consecutive"))), msg);
            }
            // After a watchdog freeze the supervisor's alert was already sent.
            if get(e, "why").and_then(Value::as_str) == Some("freeze") {
                return plain(msg);
            }
            let vars = vec![("videoIndex", num_value(n)), ("videoId", one(get(e, "id")).into()), ("reason", reason.into())];
            alert(msg, K::Skip, vars, AlertLevel::Warn)
        }
        "retry" => {
            let msg = format!("Retrying video {n_text} at {}s ({}, attempt {})", js_num(num(e, "pos").floor()), one(get(e, "why")), js_num(num(e, "retry")));
            // The watchdog-freeze event that led here already sent the alert.
            if get(e, "why").and_then(Value::as_str) == Some("freeze") {
                return plain(msg);
            }
            let vars = vec![
                ("videoIndex", num_value(n)),
                ("videoId", one(get(e, "id")).into()),
                ("errorCode", one(get(e, "why")).into()),
                ("attempt", num_value(num(e, "retry"))),
            ];
            alert(msg, K::Error, vars, AlertLevel::Warn)
        }
        "watchdog-freeze" | "watchdog-silence" => {
            let kind = if e.kind == "watchdog-freeze" { "freeze" } else { "silence" };
            let outcome = one(get(e, "outcome"));
            if kind == "silence" && outcome == "content" {
                return plain(format!("Silence on video {n_text}. Silence alone never restarts a video."));
            }
            let msg = format!("Output {kind} on video {n_text} ({outcome})");
            if outcome == "retry" || outcome == "skip" {
                return alert(msg, K::OutputFrozen, vec![("kind", kind.into()), ("videoIndex", num_value(n))], AlertLevel::Warn);
            }
            plain(msg)
        }
        "resume" => alert(
            format!("Resumed video {n_text} at {}s", js_num(num(e, "pos").floor())),
            K::Resume,
            vec![("videoIndex", num_value(n)), ("videoId", one(get(e, "id")).into())],
            AlertLevel::Info,
        ),
        "all-skipped" => critical(format!("Every video was skipped. Holding for {}s before trying again.", js_num(num(e, "holdS")))),
        "playlist-advance" => {
            let now = format!("Now playing {}", list_name(e, "to"));
            let msg = if get(e, "finished") == Some(&Value::Bool(true)) { format!("Playlist finished. {now}") } else { now };
            let missing = missing_lists(e);
            if missing.is_empty() {
                plain(msg)
            } else {
                critical(format!("{msg}. {missing} could not be loaded"))
            }
        }
        "playlist-restart" => {
            let missing = missing_lists(e);
            if !missing.is_empty() {
                return critical(format!("Playlist finished. Starting it again, because {missing} could not be loaded"));
            }
            plain(format!("Playlist finished. Starting it again ({} videos)", js_num(num(e, "count"))))
        }
        "playlist-failed" => {
            let which = list_name(e, "playlistId");
            let which = if which.is_empty() { String::new() } else { format!("{which} ") };
            let msg = format!("Playlist {which}could not be loaded: {}", one(get(e, "reason")));
            // Only a first load is critical; a failed switch keeps the old list playing.
            if is_critical {
                critical(msg)
            } else {
                plain(msg)
            }
        }
        "stream-key-unreadable" => critical(format!("The stream key cannot be read: {}", one(get(e, "reason")))),
        "stream-key-missing" => critical("No stream key is set. The stream cannot start.".into()),
        "youtube-breaker-open" => {
            // Three failed lookups also open it; only a bot check is YouTube refusing the box.
            let what = if get(e, "bot") == Some(&Value::Bool(false)) { "YouTube lookups keep failing" } else { "YouTube is blocking requests" };
            critical(format!("{what} ({}). Downloads and lookups are paused.", one(get(e, "reason"))))
        }
        "cookies-unreadable" => {
            // warn: the call went on without the cookies; critical: the calls fail on them.
            if !is_critical {
                return plain(format!("yt-dlp could not read the {} cookies and went on without them: {}", one(get(e, "browser")), one(get(e, "reason"))));
            }
            critical(format!("yt-dlp could not read the {} cookies: {}", one(get(e, "browser")), one(get(e, "reason"))))
        }
        "config-refused" => critical(format!("The supervisor refused config.json: {}", one(get(e, "reason")))),
        "youtube-breaker-closed" => plain("YouTube requests resumed"),
        "downloads-failing" => critical(format!("{} downloads failed in a row ({}).", js_num(num(e, "inARow")), one(get(e, "reason")))),
        "feeder-spawn-failed" => critical(format!("Video {n_text} could not start: {}", one(get(e, "error")))),
        "feeder-spawn-recovered" => plain(format!("Video {n_text} started after an earlier failure")),
        "supervisor-down" => critical("The supervisor has stopped answering.".into()),
        // The recovery template ("Recovery action: {step}") fits; streamRestart would read as a stream restart.
        "supervisor-back" => alert("Supervisor is answering again", K::Recovery, vec![("step", "Supervisor answering again".into())], AlertLevel::Info),
        "supervisor-missing" => critical("The supervisor program is missing from the install.".into()),
        "supervisor-launched" => plain("Supervisor launched"),
        "supervisor-version-restart" => plain(format!(
            "Restarting the supervisor: it runs {}, this is {}.",
            match get(e, "running") {
                Some(Value::String(v)) if !v.is_empty() => v.clone(),
                _ => "an unknown version".to_string(),
            },
            one(get(e, "expected"))
        )),
        // The launch helper's own event; "Supervisor launched" already covers a normal launch.
        "launched" => plain(if get(e, "breakaway") == Some(&Value::Bool(false)) { "Supervisor launched inside the caller's job and may stop with it" } else { "" }),
        // The launch guard reports each block once, so this alerts once per block, not per check.
        "supervisor-blocked-by-obs" => critical(OBS_RUNNING.into()),
        "encoder-fallback" => critical(format!("The {} encoder failed its test, so the stream uses {}.", one(get(e, "from")), one(get(e, "to")))),
        "no-encoder" => critical(NO_ENCODER.into()),
        "ffmpeg-missing" => critical(FFMPEG_MISSING.into()),
        "encoder-restored" => alert(
            format!("The {} encoder works again, so the stream uses it.", one(get(e, "to"))),
            K::Recovery,
            vec![("step", format!("Back on the {} encoder", one(get(e, "to"))).into())],
            AlertLevel::Info,
        ),
        "start" => plain("Supervisor started"),
        "shutdown" => plain("Supervisor shut down"),
        "reload" => plain("Configuration reloaded"),
        "control" => plain(format!("Control: {}", one(get(e, "cmd")))),
        "playlist" => plain(format!("Playlist loaded ({} videos)", js_num(num(e, "count")))),
        "playlist-recovered" => plain("Playlist loaded after an earlier failure"),
        "playlist-from-saved" => plain(format!("YouTube is not answering, so the saved playlist ({} videos) is used.", js_num(num(e, "count")))),
        "playlist-switch-failed" => plain(format!("Playlist switch failed: {}", one(get(e, "reason")))),
        "download-failed" => {
            // The supervisor schedules the next round; one from before that leaves retryInS out.
            let retry = num(e, "retryInS");
            let next = if retry > 0.0 { format!("; trying again in {} min", js_num((retry / 60.0).round().max(1.0))) } else { String::new() };
            plain(format!("Download of video {n_text} failed: {}{next}", one(get(e, "reason"))))
        }
        "download-low-quality" => warning(format!("Video {n_text} ({}) was downloaded below 720p: none of the player clients offered 720p or better.", one(get(e, "id")))),
        "resolve-failed" => plain(format!("Lookup of video {n_text} failed: {}", one(get(e, "reason")))),
        "url-refresh" => plain(format!("Refreshed the address of video {n_text}")),
        "slate-stall" => plain("Standby slate stalled"),
        "breaker-retarget" => plain("Moved to a playable video while YouTube is blocked"),
        "youtube-probe-failed" => plain(format!("YouTube probe failed: {}", one(get(e, "reason")))),
        "encoder-restart-requested" => plain(format!("Encoder restart requested: {}", one(get(e, "reason")))),
        "overlay-failed" => plain(format!("Overlay failed: {}", one(get(e, "error")))),
        "log-prune-failed" => plain(format!("Old logs could not be removed: {}", one(get(e, "first")))),
        "deadline" => plain(format!("Test run reached its {} hour limit", js_num(num(e, "hours")))),
        "fault" | "fault-applied" => plain(format!("Test fault: {}", one(get(e, "fault")))),
        "token-server-spawn" => plain("Token server started"),
        "token-server-adopt" => plain("Token server already running"),
        "token-server-down" => plain("Token server stopped answering"),
        "token-server-restart" => plain("Token server restarted"),
        "token-caches-cleared" => plain(if get(e, "ok") == Some(&Value::Bool(false)) {
            format!("YouTube refused {} downloads with a token, and the token server's cache could not be cleared", js_num(num(e, "refusals")))
        } else {
            format!("YouTube refused {} downloads with a token, so the token server starts over with new tokens", js_num(num(e, "refusals")))
        }),
        "token-server-spawn-failed" => {
            let error = one(get(e, "error"));
            critical_with(format!("The token server could not start: {error}"), format!("Token server could not start: {error}"))
        }
        // A kind this table does not know still reaches Discord when the supervisor calls it critical.
        _ => {
            if is_critical {
                critical(e.kind.clone())
            } else {
                plain(e.kind.clone())
            }
        }
    }
}

/// `describe` plus once-per-streak gating, so a long outage sends one alert, not one per retry.
/// Every event still gets its timeline message; only the Discord part is dropped.
#[derive(Debug, Default)]
pub struct AlertGate {
    /// An encoder alert went out since the last encoder-start.
    encoder_alerted: bool,
    spawn_alerted: bool,
    token_alerted: bool,
    /// A config-refused alert went out since the last supervisor start.
    config_alerted: bool,
    /// from>to of the last encoder-fallback alert.
    last_fallback: String,
}

impl AlertGate {
    pub fn describe(&mut self, e: &SupervisorEvent) -> Described {
        let d = describe(e);
        let drop = |d: &Described| plain(d.message.clone());
        match e.kind.as_str() {
            "encoder-exit" => {
                let failures = num(e, "failures");
                // 1 is the drop, 3 the critical; 2 and everything past 3 are timeline only.
                if failures != 1.0 && failures != 3.0 {
                    return drop(&d);
                }
                // The 48 h cut sends nothing, so its restart sends nothing either.
                if d.discord.is_some() {
                    self.encoder_alerted = true;
                }
                d
            }
            "encoder-spawn-failed" => {
                if self.spawn_alerted {
                    return drop(&d);
                }
                self.spawn_alerted = true;
                self.encoder_alerted = true;
                d
            }
            "encoder-start" => {
                let alerted = self.encoder_alerted;
                self.encoder_alerted = false;
                self.spawn_alerted = false;
                if !alerted {
                    return drop(&d);
                }
                // A first start after failed spawns has restarts 0, which describe() treats as a plain start.
                if d.discord.is_some() {
                    d
                } else {
                    alert(d.message, DiscordKind::StreamRestart, vec![("attempts", 1.into())], AlertLevel::Info)
                }
            }
            "config-refused" => {
                if self.config_alerted {
                    return drop(&d);
                }
                self.config_alerted = true;
                d
            }
            "start" => {
                self.config_alerted = false;
                d
            }
            "token-server-spawn-failed" => {
                if self.token_alerted {
                    return drop(&d);
                }
                self.token_alerted = true;
                d
            }
            "encoder-fallback" => {
                let change = format!("{}>{}", one(get(e, "from")), one(get(e, "to")));
                if change == self.last_fallback {
                    return drop(&d);
                }
                self.last_fallback = change;
                d
            }
            "encoder-restored" => {
                self.last_fallback.clear();
                d
            }
            "token-server-spawn" | "token-server-adopt" => {
                self.token_alerted = false;
                d
            }
            _ => d,
        }
    }
}

const CRITICAL_LAUNCHER: [&str; 6] = ["supervisor-down", "supervisor-missing", "supervisor-blocked-by-obs", "encoder-fallback", "no-encoder", "ffmpeg-missing"];

/// Shape a launcher callback as a SupervisorEvent, so one function routes both sources.
pub fn launcher_event(kind: &str, detail: Map<String, Value>, now: SystemTime) -> SupervisorEvent {
    let ms = now.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let level = if CRITICAL_LAUNCHER.contains(&kind) { "critical" } else { "info" };
    let mut m = Map::new();
    m.insert("ts".into(), iso(ms).into());
    m.insert("level".into(), level.into());
    m.insert("kind".into(), kind.into());
    // The detail goes last, so a key of the same name wins, as in the spread this ports.
    m.extend(detail);
    SupervisorEvent::from_map(m).unwrap_or_else(|| SupervisorEvent { ts: iso(ms), level: level.into(), kind: kind.into(), rest: Map::new() })
}

/// Tails the supervisor's daily events file; `poll` hands back each new complete line.
pub struct SupervisorEvents {
    logs_dir: PathBuf,
    clock: Arc<dyn Clock>,
    started: bool,
    date: String,
    offset: u64,
    /// The file the offset belongs to; a supervisor that switches to the new name starts it from 0.
    path: PathBuf,
    warned_date: String,
}

impl SupervisorEvents {
    /// `logs_dir` is the supervisor's own `data\logs` folder, never the control process's `logs\`.
    pub fn new(logs_dir: &Path, clock: Arc<dyn Clock>) -> SupervisorEvents {
        SupervisorEvents { logs_dir: logs_dir.to_path_buf(), clock, started: false, date: String::new(), offset: 0, path: PathBuf::new(), warned_date: String::new() }
    }

    /// No replay: begins at the end of today's file.
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.date = local_date(self.clock.now());
        self.path = events_file(&self.logs_dir, &self.date);
        self.offset = size_of(&self.path).unwrap_or(0);
    }

    /// One read pass: the complete lines written since the last one, then the same for a new day.
    pub fn poll(&mut self) -> Vec<SupervisorEvent> {
        let mut out = Vec::new();
        loop {
            if let Err(err) = self.read_new(&mut out) {
                tracing::warn!(%err, "Reading supervisor events failed");
                break;
            }
            let today = local_date(self.clock.now());
            if today == self.date {
                break;
            }
            // Yesterday is finished; a partial last line there will never complete.
            self.date = today;
            self.offset = 0;
        }
        out
    }

    fn read_new(&mut self, out: &mut Vec<SupervisorEvent>) -> std::io::Result<()> {
        let path = events_file(&self.logs_dir, &self.date);
        if path != self.path {
            self.path = path.clone();
            self.offset = 0;
        }
        let Some(size) = size_of(&path) else { return Ok(()) };
        if size < self.offset {
            self.offset = 0;
        }
        if size == self.offset {
            return Ok(());
        }
        let mut buf = vec![0u8; (size - self.offset) as usize];
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        file.read_exact(&mut buf)?;
        let Some(end) = buf.iter().rposition(|b| *b == b'\n') else { return Ok(()) };
        self.offset += end as u64 + 1;
        for line in String::from_utf8_lossy(&buf[..end]).split('\n') {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(v) => out.extend(SupervisorEvent::from_value(v)),
                Err(_) => {
                    if self.warned_date != self.date {
                        self.warned_date = self.date.clone();
                        tracing::warn!(file = %path.display(), "Skipping an unparseable supervisor event line");
                    }
                }
            }
        }
        Ok(())
    }
}

fn size_of(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}
