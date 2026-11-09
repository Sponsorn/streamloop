//! The Discord webhook notifier: templates, the 5 s batch window, criticals sent at once, the 429 wait.
//! The webhook URL is a secret, so nothing here puts it in a log line or an error message.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use streamloop_common::config::ordered::Value as Json;
use streamloop_common::config::{Discord, DiscordEvents, DiscordTemplates};
use streamloop_common::time::iso;

use crate::clock::Clock;
pub use crate::events::AlertLevel as Level;
use crate::events::{Alert, DiscordKind};
use crate::http::{Http, HttpError, HttpErrorKind, HttpRequest};

/// A template variable: a string or a number.
pub type AlertVar = serde_json::Value;

const BATCH_WINDOW: Duration = Duration::from_secs(5);
const RATE_LIMIT_DEFAULT_WAIT: Duration = Duration::from_secs(10);
const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(30);
const POST_TIMEOUT: Duration = Duration::from_secs(30);

const COLOR_INFO: f64 = 3_447_003.0;
const COLOR_WARN: f64 = 16_776_960.0;
const COLOR_ERROR: f64 = 15_158_332.0;

#[derive(Debug, Clone, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: Option<bool>,
}

/// What the Twitch checks, the event bridge and the landing send through.
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify_critical(&self, message: &str);
    async fn notify_event(&self, alert: Alert);
    async fn notify_twitch_mismatch(&self, channel: &str);
    async fn notify_twitch_restart(&self, channel: &str);
    async fn send(&self, content: &str, level: Level, fields: Option<Vec<EmbedField>>);
    /// Sends whatever is batched now and waits for the post to finish.
    async fn flush(&self);
}

/// `{name}` (ASCII letters, digits and underscore) is replaced by its variable; an unknown name stays as written.
pub fn render_template(template: &str, vars: &[(String, AlertVar)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
        if name_len > 0 && after.as_bytes().get(name_len) == Some(&b'}') {
            let name = &after[..name_len];
            match vars.iter().rev().find(|(k, _)| k == name) {
                Some((_, v)) => out.push_str(&var_text(v)),
                None => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                }
            }
            rest = &after[name_len + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn var_text(v: &AlertVar) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => n.as_f64().map_or_else(|| n.to_string(), |f| f.to_string()),
        },
        other => other.to_string(),
    }
}

fn format_uptime(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m) = (s / 3600, s % 3600 / 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {}s", s % 60)
    } else {
        format!("{s}s")
    }
}

/// `Number.prototype.toFixed(1)`: a tie rounds up, where Rust's formatter rounds it to even.
fn to_fixed_1(v: f64) -> String {
    let t = v * 10.0;
    if t.fract() == 0.5 {
        format!("{:.1}", t.ceil() / 10.0)
    } else {
        format!("{v:.1}")
    }
}

fn memory_field(total: u64, free: u64) -> EmbedField {
    const GIB: f64 = 1_073_741_824.0;
    let used = total.saturating_sub(free) as f64;
    let total = total as f64;
    let percent = if total > 0.0 { (used / total * 100.0 + 0.5).floor() } else { 0.0 };
    EmbedField { name: "RAM".into(), value: format!("{}/{} GB ({percent}%)", to_fixed_1(used / GIB), to_fixed_1(total / GIB)), inline: Some(true) }
}

/// Total and free physical memory in bytes.
#[cfg(windows)]
pub(crate) fn system_memory() -> (u64, u64) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: an all-zero MEMORYSTATUSEX is a valid value, and the call needs only its length field set.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `status` is a live, correctly sized struct.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return (0, 0);
    }
    (status.ullTotalPhys, status.ullAvailPhys)
}

#[cfg(not(windows))]
pub(crate) fn system_memory() -> (u64, u64) {
    (0, 0)
}

fn priority(l: Level) -> u8 {
    match l {
        Level::Info => 0,
        Level::Warn => 1,
        Level::Error => 2,
    }
}

fn emoji(l: Level) -> &'static str {
    match l {
        Level::Info => "\u{2139}\u{FE0F}",
        Level::Warn => "\u{26A0}\u{FE0F}",
        Level::Error => "\u{1F6A8}",
    }
}

fn template_for(t: &DiscordTemplates, kind: DiscordKind) -> &str {
    match kind {
        DiscordKind::Error => &t.error,
        DiscordKind::Skip => &t.skip,
        DiscordKind::Recovery => &t.recovery,
        DiscordKind::Critical => &t.critical,
        DiscordKind::Resume => &t.resume,
        DiscordKind::StreamDrop => &t.stream_drop,
        DiscordKind::StreamRestart => &t.stream_restart,
        DiscordKind::OutputFrozen => &t.output_frozen,
        DiscordKind::TwitchMismatch => &t.twitch_mismatch,
        DiscordKind::TwitchRestart => &t.twitch_restart,
    }
}

fn toggle(e: &DiscordEvents, kind: DiscordKind) -> bool {
    match kind {
        DiscordKind::Error => e.error,
        DiscordKind::Skip => e.skip,
        DiscordKind::Recovery => e.recovery,
        DiscordKind::Critical => e.critical,
        DiscordKind::Resume => e.resume,
        DiscordKind::StreamDrop => e.stream_drop,
        DiscordKind::StreamRestart => e.stream_restart,
        DiscordKind::OutputFrozen => e.output_frozen,
        DiscordKind::TwitchMismatch => e.twitch_mismatch,
        DiscordKind::TwitchRestart => e.twitch_restart,
    }
}

fn text(t: &str) -> Json {
    Json::String(t.to_string())
}

fn field_json(f: &EmbedField) -> Json {
    let mut o = vec![("name".to_string(), text(&f.name)), ("value".into(), text(&f.value))];
    if let Some(inline) = f.inline {
        o.push(("inline".into(), Json::Bool(inline)));
    }
    Json::Object(o)
}

/// A transport error's message can carry the request URL, so only its kind is logged.
fn failure_kind(e: &HttpError) -> String {
    match &e.kind {
        HttpErrorKind::Timeout => "timeout".into(),
        HttpErrorKind::Connect { code } => format!("connect {code}"),
        HttpErrorKind::Other => "request failed".into(),
    }
}

/// The `retry-after` wait as Node reads it with `parseFloat`: a leading number, a missing header meaning 10 s,
/// anything unreadable or negative meaning none. Capped so a hostile value cannot hold up exit.
fn retry_wait(header: Option<&str>) -> Duration {
    let Some(raw) = header.filter(|h| !h.is_empty()) else { return RATE_LIMIT_DEFAULT_WAIT };
    let secs = parse_float_prefix(raw);
    if secs.is_nan() || secs <= 0.0 {
        return Duration::ZERO;
    }
    Duration::try_from_secs_f64(secs.min(RATE_LIMIT_MAX_WAIT.as_secs_f64())).unwrap_or(RATE_LIMIT_MAX_WAIT)
}

/// `parseFloat`: the longest leading decimal number, NaN when there is none.
fn parse_float_prefix(raw: &str) -> f64 {
    let t = raw.trim_start();
    let b = t.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let int = digits(i);
    i += int;
    let mut frac = 0;
    if b.get(i) == Some(&b'.') {
        frac = digits(i + 1);
        if int > 0 || frac > 0 {
            i += 1 + frac;
        }
    }
    if int == 0 && frac == 0 {
        return f64::NAN;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp = digits(j);
        if exp > 0 {
            i = j + exp;
        }
    }
    t[..i].parse().unwrap_or(f64::NAN)
}

struct Queued {
    content: String,
    level: Level,
    fields: Option<Vec<EmbedField>>,
}

#[derive(Default)]
struct Batch {
    queue: Vec<Queued>,
    /// Bumped by every flush; a timer armed under an older number does nothing.
    generation: u64,
    timer_armed: bool,
}

struct Inner {
    discord: Discord,
    version: String,
    uptime_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    admin_url: String,
    http: Arc<dyn Http>,
    clock: Arc<dyn Clock>,
    batch: Mutex<Batch>,
    /// Held across a post and its 429 wait, so `flush` waits for a send already under way.
    sending: tokio::sync::Mutex<()>,
}

pub struct DiscordNotifier {
    inner: Arc<Inner>,
}

impl DiscordNotifier {
    pub fn new(
        discord: Discord,
        version: &str,
        uptime_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
        admin_url: &str,
        http: Arc<dyn Http>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let inner = Inner { discord, version: version.into(), uptime_ms, admin_url: admin_url.into(), http, clock, batch: Mutex::new(Batch::default()), sending: tokio::sync::Mutex::new(()) };
        Self { inner: Arc::new(inner) }
    }
}

impl Inner {
    fn enabled(&self) -> bool {
        !self.discord.webhook_url.is_empty()
    }

    fn footer_text(&self, extra: Option<String>) -> String {
        let mut parts = Vec::new();
        if !self.admin_url.is_empty() {
            parts.push(format!("Dashboard: {}", self.admin_url));
        }
        parts.push(format!("Uptime: {}", format_uptime((self.uptime_ms)())));
        parts.push(format!("v{}", self.version));
        let footer = parts.join(" | ");
        match extra {
            Some(x) => format!("{x} | {footer}"),
            None => footer,
        }
    }

    /// The webhook body in Node's key order.
    fn body(&self, messages: &[Queued]) -> Json {
        let highest = messages.iter().map(|m| m.level).fold(Level::Info, |max, l| if priority(l) > priority(max) { l } else { max });
        let color = match highest {
            Level::Info => COLOR_INFO,
            Level::Warn => COLOR_WARN,
            Level::Error => COLOR_ERROR,
        };
        let batched = messages.len() > 1;
        let description = messages
            .iter()
            .map(|m| if batched { format!("{} {}", emoji(m.level), m.content) } else { m.content.clone() })
            .collect::<Vec<_>>()
            .join("\n\n");
        let footer_extra = batched.then(|| format!("{} events", messages.len()));
        let now_ms = self.clock.now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);

        let mut embed = vec![
            ("title".to_string(), Json::String(format!("{} StreamLoop", emoji(highest)))),
            ("description".into(), Json::String(description)),
            ("color".into(), Json::Number(color)),
            ("timestamp".into(), Json::String(iso(now_ms))),
            ("footer".into(), Json::Object(vec![("text".into(), Json::String(self.footer_text(footer_extra)))])),
        ];
        if let Some(fields) = messages.first().filter(|_| !batched).and_then(|m| m.fields.as_ref()) {
            embed.push(("fields".into(), Json::Array(fields.iter().map(field_json).collect())));
        }

        let mut body = vec![("embeds".to_string(), Json::Array(vec![Json::Object(embed)]))];
        if !self.discord.bot_name.is_empty() {
            body.push(("username".into(), text(&self.discord.bot_name)));
        }
        if !self.discord.avatar_url.is_empty() {
            body.push(("avatar_url".into(), text(&self.discord.avatar_url)));
        }
        if highest == Level::Error && !self.discord.role_ping.is_empty() {
            body.push(("content".into(), Json::String(format!("<@&{}>", self.discord.role_ping))));
        }
        Json::Object(body)
    }

    async fn flush(&self) {
        let _sending = self.sending.lock().await;
        let messages = {
            let mut b = self.batch.lock().unwrap();
            b.generation += 1;
            b.timer_armed = false;
            std::mem::take(&mut b.queue)
        };
        if messages.is_empty() {
            return;
        }
        let req = HttpRequest {
            method: "POST".into(),
            url: self.discord.webhook_url.clone(),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(self.body(&messages).to_compact().into_bytes()),
            timeout: Some(POST_TIMEOUT),
        };
        match self.http.send(req).await {
            Ok(res) if res.status == 429 => {
                let wait = retry_wait(res.header("retry-after"));
                tracing::warn!(retry_after_ms = wait.as_millis() as u64, "Discord rate limited, waiting");
                tokio::time::sleep(wait).await;
            }
            Ok(res) if !(200..300).contains(&res.status) => tracing::error!(status = res.status, "Discord webhook failed"),
            Ok(_) => {}
            Err(e) => tracing::error!(cause = failure_kind(&e), "Discord webhook error"),
        }
    }

    async fn enqueue(self: &Arc<Self>, content: &str, level: Level, fields: Option<Vec<EmbedField>>) {
        if !self.enabled() {
            return;
        }
        let arm = {
            let mut b = self.batch.lock().unwrap();
            b.queue.push(Queued { content: content.into(), level, fields });
            if level == Level::Error || b.timer_armed {
                None
            } else {
                b.timer_armed = true;
                Some(b.generation)
            }
        };
        if level == Level::Error {
            self.flush().await;
        } else if let Some(generation) = arm {
            let me = Arc::clone(self);
            tokio::spawn(async move {
                tokio::time::sleep(BATCH_WINDOW).await;
                let current = me.batch.lock().unwrap().generation == generation;
                if current {
                    me.flush().await;
                }
            });
        }
    }
}

#[async_trait]
impl Notifier for DiscordNotifier {
    async fn notify_critical(&self, message: &str) {
        let d = &self.inner.discord;
        if !d.events.critical {
            return;
        }
        let content = render_template(&d.templates.critical, &[("message".into(), message.into())]);
        let (total, free) = system_memory();
        let fields = vec![EmbedField { name: "Status".into(), value: message.into(), inline: Some(false) }, memory_field(total, free)];
        self.inner.enqueue(&content, Level::Error, Some(fields)).await;
    }

    async fn notify_event(&self, alert: Alert) {
        let d = &self.inner.discord;
        if !toggle(&d.events, alert.kind) {
            return;
        }
        let content = render_template(template_for(&d.templates, alert.kind), &alert.vars);
        self.inner.enqueue(&content, alert.level, None).await;
    }

    async fn notify_twitch_mismatch(&self, channel: &str) {
        let d = &self.inner.discord;
        if !d.events.twitch_mismatch {
            return;
        }
        let content = render_template(&d.templates.twitch_mismatch, &[("channel".into(), channel.into())]);
        self.inner.enqueue(&content, Level::Warn, None).await;
    }

    async fn notify_twitch_restart(&self, channel: &str) {
        let d = &self.inner.discord;
        if !d.events.twitch_restart {
            return;
        }
        let content = render_template(&d.templates.twitch_restart, &[("channel".into(), channel.into())]);
        self.inner.enqueue(&content, Level::Info, None).await;
    }

    async fn send(&self, content: &str, level: Level, fields: Option<Vec<EmbedField>>) {
        self.inner.enqueue(content, level, fields).await;
    }

    async fn flush(&self) {
        self.inner.flush().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_reads_like_parse_float_and_is_capped() {
        let secs = |h: Option<&str>| retry_wait(h).as_secs_f64();
        assert_eq!(secs(None), 10.0);
        assert_eq!(secs(Some("2abc")), 2.0);
        assert_eq!(secs(Some(" 1.5")), 1.5);
        assert_eq!(secs(Some("-3")), 0.0);
        assert_eq!(secs(Some("abc")), 0.0);
        assert_eq!(secs(Some("3600")), 30.0);
        assert_eq!(secs(Some("1e300")), 30.0);
    }

    #[test]
    fn to_fixed_rounds_a_tie_up_like_javascript() {
        assert_eq!(to_fixed_1(8.25), "8.3");
        assert_eq!(to_fixed_1(8.24), "8.2");
    }

    #[test]
    fn memory_field_reads_used_over_total_with_a_percentage() {
        // Arrange
        let gib = 1_073_741_824u64;
        // Act
        let f = memory_field(16 * gib, 4 * gib);
        // Assert
        assert_eq!((f.name.as_str(), f.value.as_str(), f.inline), ("RAM", "12.0/16.0 GB (75%)", Some(true)));
    }
}
