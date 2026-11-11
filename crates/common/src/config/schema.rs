//! The Zod schema of `src/server/config.ts`: the keys, defaults and bounds, and the key order a
//! write uses.

use std::fmt;

use super::ordered::Value;
use super::Issue;
use crate::encoder::Encoder;

pub const DEFAULT_TEMPLATES: [(&str, &str); 10] = [
    ("error", "Playback error **{errorCode}** on video #{videoIndex} (`{videoId}`)\nRetry attempt: {attempt}"),
    ("skip", "Skipping video #{videoIndex} (`{videoId}`)\nReason: {reason}"),
    ("recovery", "Recovery action: **{step}**"),
    ("critical", "**CRITICAL:** {message}"),
    ("resume", "Playback resumed at video #{videoIndex} (`{videoId}`)"),
    ("streamDrop", "Stream stopped unexpectedly — attempting restart (attempt {attempt}/{maxAttempts})"),
    ("streamRestart", "Stream restarted successfully after {attempts} attempt(s)"),
    ("outputFrozen", "Output frozen ({kind}) on video #{videoIndex}"),
    ("twitchMismatch", "Twitch liveness mismatch — the encoder reports streaming but **{channel}** is offline on Twitch. Restarting stream."),
    ("twitchRestart", "Stream restarted after Twitch liveness mismatch on **{channel}**"),
];

pub const TEMPLATE_VARIABLES: [(&str, &[&str]); 10] = [
    ("error", &["videoIndex", "videoId", "errorCode", "attempt"]),
    ("skip", &["videoIndex", "videoId", "reason"]),
    ("recovery", &["step"]),
    ("critical", &["message"]),
    ("resume", &["videoIndex", "videoId"]),
    ("streamDrop", &["attempt", "maxAttempts"]),
    ("streamRestart", &["attempts"]),
    ("outputFrozen", &["kind", "videoIndex"]),
    ("twitchMismatch", &["channel"]),
    ("twitchRestart", &["channel"]),
];

const DEFAULT_INGEST_URL: &str = "rtmp://live.twitch.tv/app";
const DEFAULT_PLAYER_CLIENT: &str = "mweb,web_embedded";
const ENCODERS: &str = "h264_nvenc, h264_amf, h264_qsv, libx264";
/// Zod's `int()` takes only safe integers.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Clone, PartialEq)]
pub struct Config {
    pub port: u64,
    pub playlists: Vec<Playlist>,
    pub discord: Discord,
    pub supervisor: Supervisor,
    pub auto_update_check: bool,
    pub update_check_interval_ms: u64,
    pub auto_update_ytdlp: bool,
    pub auto_open_admin: bool,
    pub auto_start_stream: bool,
    pub twitch_client_id: String,
    pub twitch_client_secret: String,
    pub twitch_channel: String,
    pub twitch_liveness_enabled: bool,
    pub twitch_poll_interval_ms: u64,
    pub ytdl_cookies_from_browser: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Clone, PartialEq)]
pub struct Discord {
    pub webhook_url: String,
    pub bot_name: String,
    pub avatar_url: String,
    pub role_ping: String,
    pub events: DiscordEvents,
    pub templates: DiscordTemplates,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscordEvents {
    pub error: bool,
    pub skip: bool,
    pub recovery: bool,
    pub critical: bool,
    pub resume: bool,
    pub stream_drop: bool,
    pub stream_restart: bool,
    pub output_frozen: bool,
    pub twitch_mismatch: bool,
    pub twitch_restart: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiscordTemplates {
    pub error: String,
    pub skip: String,
    pub recovery: String,
    pub critical: String,
    pub resume: String,
    pub stream_drop: String,
    pub stream_restart: String,
    pub output_frozen: String,
    pub twitch_mismatch: String,
    pub twitch_restart: String,
}

#[derive(Clone, PartialEq)]
pub struct Supervisor {
    pub ingest_url: String,
    /// DPAPI blob; the plain stream key is never stored.
    pub stream_key_protected: String,
    pub encoder: Encoder,
    /// What the user chose; `encoder` may hold a fallback until this works again.
    pub preferred_encoder: Option<Encoder>,
    pub cache_budget_mb: u64,
    pub read_ahead_seconds: f64,
    pub player_client: String,
    pub overlay: bool,
    pub freeze_seconds: f64,
    pub silence_seconds: f64,
    /// Keys a newer supervisor knows, in file order; they survive every load and save.
    pub extra: Vec<(String, Value)>,
}

const SUPERVISOR_KEYS: [&str; 10] = [
    "ingestUrl", "streamKeyProtected", "encoder", "preferredEncoder", "cacheBudgetMb",
    "readAheadSeconds", "playerClient", "overlay", "freezeSeconds", "silenceSeconds",
];

const EVENT_KEYS: [&str; 10] = [
    "error", "skip", "recovery", "critical", "resume", "streamDrop", "streamRestart", "outputFrozen", "twitchMismatch", "twitchRestart",
];

impl DiscordEvents {
    fn from_flags(f: [bool; 10]) -> DiscordEvents {
        let [error, skip, recovery, critical, resume, stream_drop, stream_restart, output_frozen, twitch_mismatch, twitch_restart] = f;
        DiscordEvents { error, skip, recovery, critical, resume, stream_drop, stream_restart, output_frozen, twitch_mismatch, twitch_restart }
    }

    fn flags(&self) -> [bool; 10] {
        let e = self;
        [e.error, e.skip, e.recovery, e.critical, e.resume, e.stream_drop, e.stream_restart, e.output_frozen, e.twitch_mismatch, e.twitch_restart]
    }
}

impl DiscordTemplates {
    fn from_texts(t: [String; 10]) -> DiscordTemplates {
        let [error, skip, recovery, critical, resume, stream_drop, stream_restart, output_frozen, twitch_mismatch, twitch_restart] = t;
        DiscordTemplates { error, skip, recovery, critical, resume, stream_drop, stream_restart, output_frozen, twitch_mismatch, twitch_restart }
    }

    fn texts(&self) -> [&str; 10] {
        let t = self;
        [&t.error, &t.skip, &t.recovery, &t.critical, &t.resume, &t.stream_drop, &t.stream_restart, &t.output_frozen, &t.twitch_mismatch, &t.twitch_restart]
    }
}

type Object = [(String, Value)];

fn field<'a>(o: &'a Object, key: &str) -> Option<&'a Value> {
    o.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn kind(v: Option<&Value>) -> &'static str {
    match v {
        None => "nothing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "true or false",
        Some(Value::Number(_)) => "a number",
        Some(Value::String(_)) => "text",
        Some(Value::Array(_)) => "a list",
        Some(Value::Object(_)) => "an object",
    }
}

/// `String.prototype.trim`'s whitespace, which differs from Rust's at U+0085 and U+FEFF.
fn js_blank(s: &str) -> bool {
    s.chars().all(|c| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

enum Bound {
    AtLeast(f64),
    AboveZero,
}

/// Collects every refused field, in schema order, the way Zod reports all issues at once.
#[derive(Default)]
struct Check {
    issues: Vec<Issue>,
}

impl Check {
    fn fail(&mut self, prefix: &str, key: &str, message: String) {
        let path = if prefix.is_empty() { key.to_string() } else { format!("{prefix}.{key}") };
        self.issues.push(Issue { path, message });
    }

    fn wrong(&mut self, prefix: &str, key: &str, want: &str, got: Option<&Value>) {
        self.fail(prefix, key, format!("expected {want}, got {}", kind(got)));
    }

    /// A missing section takes all its defaults; one that is not an object is refused.
    fn section<'a>(&mut self, o: &'a Object, prefix: &str, key: &str) -> &'a Object {
        match field(o, key) {
            None => &[],
            Some(Value::Object(s)) => s,
            v => {
                self.wrong(prefix, key, "an object", v);
                &[]
            }
        }
    }

    fn text(&mut self, o: &Object, prefix: &str, key: &str, default: &str) -> String {
        match field(o, key) {
            None => default.into(),
            Some(Value::String(s)) => s.clone(),
            v => {
                self.wrong(prefix, key, "text", v);
                default.into()
            }
        }
    }

    /// A blank value means the default, so clearing a field cannot leave the encoder without a target.
    fn text_blank_is_default(&mut self, o: &Object, prefix: &str, key: &str, default: &str) -> String {
        match field(o, key) {
            Some(Value::String(s)) if js_blank(s) => default.into(),
            _ => self.text(o, prefix, key, default),
        }
    }

    fn flag(&mut self, o: &Object, prefix: &str, key: &str, default: bool) -> bool {
        match field(o, key) {
            None => default,
            Some(Value::Bool(b)) => *b,
            v => {
                self.wrong(prefix, key, "true or false", v);
                default
            }
        }
    }

    fn number(&mut self, o: &Object, prefix: &str, key: &str, default: f64, whole: bool, bound: Bound) -> f64 {
        let n = match field(o, key) {
            None => return default,
            Some(Value::Number(n)) if n.is_finite() => *n,
            v => {
                self.wrong(prefix, key, if whole { "a whole number" } else { "a number" }, v);
                return default;
            }
        };
        let shown = super::ordered::js_number(n);
        if whole && !(n.fract() == 0.0 && n.abs() <= MAX_SAFE_INTEGER) {
            self.fail(prefix, key, format!("must be a whole number (got {shown})"));
        } else {
            match bound {
                Bound::AtLeast(min) if n < min => self.fail(prefix, key, format!("must be at least {} (got {shown})", super::ordered::js_number(min))),
                Bound::AboveZero if n <= 0.0 => self.fail(prefix, key, format!("must be more than 0 (got {shown})")),
                _ => {}
            }
        }
        n
    }

    fn whole(&mut self, o: &Object, prefix: &str, key: &str, default: u64, bound: Bound) -> u64 {
        self.number(o, prefix, key, default as f64, true, bound) as u64
    }

    /// None when the key is absent or refused.
    fn encoder(&mut self, o: &Object, prefix: &str, key: &str) -> Option<Encoder> {
        match field(o, key) {
            None => None,
            Some(Value::String(s)) => {
                let e = Encoder::parse(s);
                if e.is_none() {
                    self.fail(prefix, key, format!("unknown encoder \"{s}\" ({ENCODERS})"));
                }
                e
            }
            v => {
                self.wrong(prefix, key, &format!("one of {ENCODERS}"), v);
                None
            }
        }
    }
}

fn playlists(c: &mut Check, root: &Object) -> Vec<Playlist> {
    let list = match field(root, "playlists") {
        Some(Value::Array(a)) => a,
        v => {
            c.wrong("", "playlists", "a list", v);
            return vec![];
        }
    };
    if list.is_empty() {
        c.fail("", "playlists", "needs at least one playlist".into());
    }
    let mut out = vec![];
    for (i, p) in list.iter().enumerate() {
        let Value::Object(o) = p else {
            c.wrong("playlists", &i.to_string(), "an object", Some(p));
            continue;
        };
        let prefix = format!("playlists.{i}");
        let id = match field(o, "id") {
            Some(Value::String(s)) if s.is_empty() => {
                c.fail(&prefix, "id", "must not be empty".into());
                String::new()
            }
            Some(Value::String(s)) => s.clone(),
            v => {
                c.wrong(&prefix, "id", "text", v);
                String::new()
            }
        };
        let name = match field(o, "name") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            v => {
                c.wrong(&prefix, "name", "text", v);
                None
            }
        };
        out.push(Playlist { id, name });
    }
    out
}

fn discord(c: &mut Check, root: &Object) -> Discord {
    let d = c.section(root, "", "discord");
    let p = "discord";
    let webhook_url = c.text(d, p, "webhookUrl", "");
    let bot_name = c.text(d, p, "botName", "");
    let avatar_url = c.text(d, p, "avatarUrl", "");
    let role_ping = c.text(d, p, "rolePing", "");
    let events = c.section(d, p, "events");
    let flags = EVENT_KEYS.map(|k| c.flag(events, "discord.events", k, true));
    let templates = c.section(d, p, "templates");
    let texts = DEFAULT_TEMPLATES.map(|(k, default)| c.text(templates, "discord.templates", k, default));
    Discord {
        webhook_url,
        bot_name,
        avatar_url,
        role_ping,
        events: DiscordEvents::from_flags(flags),
        templates: DiscordTemplates::from_texts(texts),
    }
}

fn supervisor(c: &mut Check, root: &Object) -> Supervisor {
    let s = c.section(root, "", "supervisor");
    let p = "supervisor";
    Supervisor {
        ingest_url: c.text_blank_is_default(s, p, "ingestUrl", DEFAULT_INGEST_URL),
        stream_key_protected: c.text(s, p, "streamKeyProtected", ""),
        encoder: c.encoder(s, p, "encoder").unwrap_or(Encoder::Nvenc),
        preferred_encoder: c.encoder(s, p, "preferredEncoder"),
        cache_budget_mb: c.whole(s, p, "cacheBudgetMb", 300, Bound::AtLeast(50.0)),
        read_ahead_seconds: c.number(s, p, "readAheadSeconds", 20.0, false, Bound::AtLeast(2.0)),
        player_client: c.text_blank_is_default(s, p, "playerClient", DEFAULT_PLAYER_CLIENT),
        overlay: c.flag(s, p, "overlay", true),
        freeze_seconds: c.number(s, p, "freezeSeconds", 20.0, false, Bound::AtLeast(5.0)),
        silence_seconds: c.number(s, p, "silenceSeconds", 30.0, false, Bound::AtLeast(5.0)),
        extra: s.iter().filter(|(k, _)| !SUPERVISOR_KEYS.contains(&k.as_str())).cloned().collect(),
    }
}

/// Fills defaults and checks bounds. Unknown keys are dropped except inside `supervisor`.
pub(crate) fn validate(root: &Object) -> Result<Config, Vec<Issue>> {
    let mut c = Check::default();
    let config = Config {
        port: c.whole(root, "", "port", 7654, Bound::AboveZero),
        playlists: playlists(&mut c, root),
        discord: discord(&mut c, root),
        supervisor: supervisor(&mut c, root),
        auto_update_check: c.flag(root, "", "autoUpdateCheck", true),
        update_check_interval_ms: c.whole(root, "", "updateCheckIntervalMs", 21_600_000, Bound::AboveZero),
        auto_update_ytdlp: c.flag(root, "", "autoUpdateYtdlp", true),
        auto_open_admin: c.flag(root, "", "autoOpenAdmin", true),
        auto_start_stream: c.flag(root, "", "autoStartStream", true),
        twitch_client_id: c.text(root, "", "twitchClientId", ""),
        twitch_client_secret: c.text(root, "", "twitchClientSecret", ""),
        twitch_channel: c.text(root, "", "twitchChannel", ""),
        twitch_liveness_enabled: c.flag(root, "", "twitchLivenessEnabled", false),
        twitch_poll_interval_ms: c.whole(root, "", "twitchPollIntervalMs", 60_000, Bound::AtLeast(30_000.0)),
        ytdl_cookies_from_browser: c.text(root, "", "ytdlCookiesFromBrowser", ""),
    };
    if c.issues.is_empty() {
        Ok(config)
    } else {
        Err(c.issues)
    }
}

/// The config in schema order, with `preferredEncoder` only when set and the unknown supervisor keys last.
pub(crate) fn to_value(c: &Config) -> Value {
    fn obj(entries: Vec<(&str, Value)>) -> Value {
        Value::Object(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    let text = |s: &str| Value::String(s.to_string());
    let num = |n: u64| Value::Number(n as f64);
    let playlists = c
        .playlists
        .iter()
        .map(|p| {
            let mut entries = vec![("id", text(&p.id))];
            if let Some(name) = &p.name {
                entries.push(("name", text(name)));
            }
            obj(entries)
        })
        .collect();
    let d = &c.discord;
    let events = obj(EVENT_KEYS.iter().copied().zip(d.events.flags().map(Value::Bool)).collect());
    let templates = obj(DEFAULT_TEMPLATES.iter().map(|(k, _)| *k).zip(d.templates.texts().map(text)).collect());
    let s = &c.supervisor;
    let mut supervisor = vec![
        ("ingestUrl", text(&s.ingest_url)),
        ("streamKeyProtected", text(&s.stream_key_protected)),
        ("encoder", text(s.encoder.as_str())),
    ];
    if let Some(e) = s.preferred_encoder {
        supervisor.push(("preferredEncoder", text(e.as_str())));
    }
    supervisor.extend([
        ("cacheBudgetMb", num(s.cache_budget_mb)),
        ("readAheadSeconds", Value::Number(s.read_ahead_seconds)),
        ("playerClient", text(&s.player_client)),
        ("overlay", Value::Bool(s.overlay)),
        ("freezeSeconds", Value::Number(s.freeze_seconds)),
        ("silenceSeconds", Value::Number(s.silence_seconds)),
    ]);
    supervisor.extend(s.extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
    obj(vec![
        ("port", num(c.port)),
        ("playlists", Value::Array(playlists)),
        (
            "discord",
            obj(vec![
                ("webhookUrl", text(&d.webhook_url)),
                ("botName", text(&d.bot_name)),
                ("avatarUrl", text(&d.avatar_url)),
                ("rolePing", text(&d.role_ping)),
                ("events", events),
                ("templates", templates),
            ]),
        ),
        ("supervisor", obj(supervisor)),
        ("autoUpdateCheck", Value::Bool(c.auto_update_check)),
        ("updateCheckIntervalMs", num(c.update_check_interval_ms)),
        ("autoUpdateYtdlp", Value::Bool(c.auto_update_ytdlp)),
        ("autoOpenAdmin", Value::Bool(c.auto_open_admin)),
        ("autoStartStream", Value::Bool(c.auto_start_stream)),
        ("twitchClientId", text(&c.twitch_client_id)),
        ("twitchClientSecret", text(&c.twitch_client_secret)),
        ("twitchChannel", text(&c.twitch_channel)),
        ("twitchLivenessEnabled", Value::Bool(c.twitch_liveness_enabled)),
        ("twitchPollIntervalMs", num(c.twitch_poll_interval_ms)),
        ("ytdlCookiesFromBrowser", text(&c.ytdl_cookies_from_browser)),
    ])
}

/// Secrets show as `***` (or empty when unset), so a `{:?}` can never print one.
fn masked(s: &str) -> &'static str {
    if s.is_empty() {
        ""
    } else {
        "***"
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Config")
            .field("port", &self.port)
            .field("playlists", &self.playlists)
            .field("discord", &self.discord)
            .field("supervisor", &self.supervisor)
            .field("auto_update_check", &self.auto_update_check)
            .field("update_check_interval_ms", &self.update_check_interval_ms)
            .field("auto_update_ytdlp", &self.auto_update_ytdlp)
            .field("auto_open_admin", &self.auto_open_admin)
            .field("auto_start_stream", &self.auto_start_stream)
            .field("twitch_client_id", &self.twitch_client_id)
            .field("twitch_client_secret", &masked(&self.twitch_client_secret))
            .field("twitch_channel", &self.twitch_channel)
            .field("twitch_liveness_enabled", &self.twitch_liveness_enabled)
            .field("twitch_poll_interval_ms", &self.twitch_poll_interval_ms)
            .field("ytdl_cookies_from_browser", &self.ytdl_cookies_from_browser)
            .finish()
    }
}

impl fmt::Debug for Discord {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Discord")
            .field("webhook_url", &masked(&self.webhook_url))
            .field("bot_name", &self.bot_name)
            .field("avatar_url", &self.avatar_url)
            .field("role_ping", &self.role_ping)
            .field("events", &self.events)
            .field("templates", &self.templates)
            .finish()
    }
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("ingest_url", &self.ingest_url)
            .field("stream_key_protected", &masked(&self.stream_key_protected))
            .field("encoder", &self.encoder)
            .field("preferred_encoder", &self.preferred_encoder)
            .field("cache_budget_mb", &self.cache_budget_mb)
            .field("read_ahead_seconds", &self.read_ahead_seconds)
            .field("player_client", &self.player_client)
            .field("overlay", &self.overlay)
            .field("freeze_seconds", &self.freeze_seconds)
            .field("silence_seconds", &self.silence_seconds)
            .field("extra", &self.extra)
            .finish()
    }
}
