//! The log line format of the Node backend (`file-log-transport.js`) on top of `tracing`: the console and
//! `streamloop-DATE.log` get the same lines, and files older than the retention period are removed.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde_json::Map;
use streamloop_common::config::ordered::Value;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

const FILE_PREFIX: &str = "streamloop-";
const RETENTION_DAYS: u64 = 7;

/// A wall-clock time in the machine's zone, down to the millisecond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millisecond: u32,
}

impl LocalTime {
    pub fn from_system(t: SystemTime) -> Self {
        let (year, month, day, hour, minute, second) = streamloop_common::time::local_parts(t);
        let millisecond = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_millis());
        Self { year, month, day, hour, minute, second, millisecond }
    }
}

fn level_label(level: u8) -> String {
    match level {
        10 => "TRACE".into(),
        20 => "DEBUG".into(),
        30 => "INFO".into(),
        40 => "WARN".into(),
        50 => "ERROR".into(),
        60 => "FATAL".into(),
        n => format!("LVL{n}"),
    }
}

/// `[YYYY-MM-DD HH:mm:ss.SSS] LEVEL: msg {extras}`, extras in the order given.
pub fn format_line_ordered(level: u8, msg: &str, extras: &[(String, Value)], local: LocalTime) -> String {
    let mut line = format!(
        "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}] {}: {}",
        local.year,
        local.month,
        local.day,
        local.hour,
        local.minute,
        local.second,
        local.millisecond,
        level_label(level),
        msg
    );
    if !extras.is_empty() {
        line.push_str(" {");
        for (i, (k, v)) in extras.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            line.push_str(&Value::String(k.clone()).to_compact());
            line.push(':');
            line.push_str(&v.to_compact());
        }
        line.push('}');
    }
    line
}

/// As `format_line_ordered` for a map, whose keys come out sorted.
pub fn format_line(level: u8, msg: &str, extras: Option<&Map<String, serde_json::Value>>, local: LocalTime) -> String {
    let list: Vec<(String, Value)> = extras.map(|m| m.iter().map(|(k, v)| (k.clone(), Value::from(v))).collect()).unwrap_or_default();
    format_line_ordered(level, msg, &list, local)
}

/// Deletes `<prefix>*<suffix>` files in `dir` last written more than `days` before `now`. A missing folder or a
/// file that will not go is skipped.
pub fn cleanup_old_files(dir: &Path, prefix: &str, suffix: &str, days: u64, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let Some(cutoff) = now.checked_sub(Duration::from_secs(days * 24 * 60 * 60)) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(prefix) || !name.ends_with(suffix) {
            continue;
        }
        if entry.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < cutoff) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[derive(Default)]
struct FieldCollector {
    msg: String,
    extras: Vec<(String, Value)>,
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.msg = format!("{value:?}");
        } else {
            self.extras.push((field.name().to_string(), Value::String(format!("{value:?}"))));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.msg = value.to_string();
        } else {
            self.extras.push((field.name().to_string(), Value::String(value.to_string())));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.extras.push((field.name().to_string(), Value::Number(value as f64)));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.extras.push((field.name().to_string(), Value::Number(value as f64)));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.extras.push((field.name().to_string(), Value::Number(value)));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.extras.push((field.name().to_string(), Value::Bool(value)));
    }
}

struct DailyFile {
    dir: PathBuf,
    day: String,
    file: Option<File>,
}

impl DailyFile {
    fn write_line(&mut self, now: SystemTime, line: &str) {
        let day = streamloop_common::time::local_date(now);
        if day != self.day || self.file.is_none() {
            if day != self.day {
                cleanup_old_files(&self.dir, FILE_PREFIX, ".log", RETENTION_DAYS, now);
            }
            self.day = day;
            self.file = OpenOptions::new().create(true).append(true).open(self.dir.join(format!("{FILE_PREFIX}{}.log", self.day))).ok();
        }
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Writes each event as one formatted line to the console and to the day's log file.
struct LineLayer {
    file: Mutex<DailyFile>,
}

impl<S: Subscriber> Layer<S> for LineLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::TRACE => 10,
            Level::DEBUG => 20,
            Level::INFO => 30,
            Level::WARN => 40,
            Level::ERROR => 50,
        };
        let mut fields = FieldCollector::default();
        event.record(&mut fields);
        let now = SystemTime::now();
        let line = format_line_ordered(level, &fields.msg, &fields.extras, LocalTime::from_system(now));
        println!("{line}");
        if let Ok(mut f) = self.file.lock() {
            f.write_line(now, &line);
        }
    }
}

/// Which targets log at which level: our own crates at `level`, every library (hyper, tokio, rustls, reqwest, h2, tower)
/// at WARN, so a library's debug logging cannot carry a URL or a key into the log.
pub fn target_filter(level: tracing::level_filters::LevelFilter) -> tracing_subscriber::filter::Targets {
    use tracing::level_filters::LevelFilter;
    tracing_subscriber::filter::Targets::new()
        .with_default(LevelFilter::WARN)
        .with_target("streamloop", level)
        .with_target("streamloop_control", level)
        .with_target("streamloop_common", level)
        .with_target("streamloop_supervisor", level)
}

/// Installs the global subscriber: lines to the console and to `logs_dir`, seven days kept, level from `LOG_LEVEL`
/// (default info) for our own targets. A second call leaves the first subscriber in place.
///
/// Convention for errors: log the error's `Display` text as an `err` field, `warn!(err = %e, "Discord post failed")`,
/// and never the `Debug` of a request, a URL with a token in it, or a body.
pub fn init(logs_dir: &Path) {
    let _ = fs::create_dir_all(logs_dir);
    cleanup_old_files(logs_dir, FILE_PREFIX, ".log", RETENTION_DAYS, SystemTime::now());
    let level = std::env::var("LOG_LEVEL").ok().and_then(|v| v.parse::<tracing::level_filters::LevelFilter>().ok()).unwrap_or(tracing::level_filters::LevelFilter::INFO);
    let layer = LineLayer { file: Mutex::new(DailyFile { dir: logs_dir.to_path_buf(), day: String::new(), file: None }) };
    let subscriber = tracing_subscriber::registry().with(layer.with_filter(target_filter(level)));
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::level_filters::LevelFilter;

    #[test]
    fn libraries_log_at_warn_while_our_crates_follow_the_level() {
        // Arrange
        let filter = target_filter(LevelFilter::DEBUG);
        // Act
        let hyper_debug = filter.would_enable("hyper::proto", &Level::DEBUG);
        let hyper_warn = filter.would_enable("hyper::proto", &Level::WARN);
        let ours = filter.would_enable("streamloop_control::api", &Level::DEBUG);
        // Assert
        assert!(!hyper_debug);
        assert!(hyper_warn);
        assert!(ours);
    }
}
