//! The supervisor's logs under `data\logs\`, bounded for 24/7 running. Growing files are daily
//! (`events-2026-10-23.jsonl`), ffmpeg process logs are one file per process, `run.json` is a single file.
//! Days are local time, so a name matches the clock on the box.

use crate::config;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Days a daily file is kept, counted from the date in its name.
pub const KEEP_DAYS: u32 = 7;
/// Per-process ffmpeg logs kept, newest first (2.x kept the last 10 mpv logs).
pub const KEEP_PROCESS_LOGS: usize = 10;
/// Bases of the daily files. Anything else in the folder is never pruned.
const DAILY: [&str; 9] = ["events", "seams", "controls", "encoder", "downloads", "deletes", "ytdlp", "buffer", "tokenserver"];
/// Prefixes of the per-process logs: `encoder-<stamp>-<generation>.log`, `feeder-<stamp>-<seq>.log`.
const PROCESS: [&str; 2] = ["encoder", "feeder"];

pub fn daily_name(base: &str, ext: &str, date: &str) -> String {
    format!("{base}-{date}.{ext}")
}

/// Local (year, month, day, hour, minute, second).
#[cfg(windows)]
fn local_parts() -> (i64, u32, u32, u32, u32, u32) {
    let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    (t.wYear.into(), t.wMonth.into(), t.wDay.into(), t.wHour.into(), t.wMinute.into(), t.wSecond.into())
}

#[cfg(not(windows))]
fn local_parts() -> (i64, u32, u32, u32, u32, u32) {
    // ponytail: UTC off Windows; the supervisor only ships on Windows.
    let s = (crate::unix_ms() / 1000) as i64;
    let (y, m, d) = civil(s.div_euclid(86_400));
    let r = s.rem_euclid(86_400) as u32;
    (y, m, d, r / 3600, r / 60 % 60, r % 60)
}

pub fn local_date() -> String {
    let (y, m, d, ..) = local_parts();
    format!("{y:04}-{m:02}-{d:02}")
}

fn local_stamp() -> String {
    let (y, m, d, h, mi, s) = local_parts();
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Howard Hinnant's civil_from_days.
#[cfg(not(windows))]
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Days since 1970-01-01 for "YYYY-MM-DD"; None for anything that is not a real-looking date.
fn days_of(date: &str) -> Option<i64> {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (date[..4].parse().ok()?, date[5..7].parse().ok()?, date[8..].parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    Some(era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468)
}

/// The date in a daily file name (`<base>-YYYY-MM-DD.<ext>`), for a known base only.
fn daily_date(name: &str) -> Option<&str> {
    let (stem, _ext) = name.rsplit_once('.')?;
    let at = stem.len().checked_sub(11)?;
    let (base, date) = (stem.get(..at)?, stem.get(at + 1..)?);
    (stem.as_bytes()[at] == b'-' && DAILY.contains(&base) && days_of(date).is_some()).then_some(date)
}

fn is_process_log(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".log") else { return false };
    PROCESS.iter().any(|p| {
        // <prefix>-YYYYMMDD-HHMMSS-<n>
        stem.strip_prefix(p).and_then(|r| r.strip_prefix('-')).is_some_and(|r| r.len() > 16 && r.as_bytes()[..15].iter().enumerate().all(|(i, c)| if i == 8 { *c == b'-' } else { c.is_ascii_digit() }) && r.as_bytes()[15] == b'-')
    })
}

/// Deletes daily files whose name date is more than `keep_days` before `today` ("YYYY-MM-DD") and all but
/// the newest `KEEP_PROCESS_LOGS` process logs. Files that match neither pattern, `state.json` and
/// `run.json` among them, are left alone. Returns one message per failure; the caller logs them once.
pub fn prune(dir: &Path, keep_days: u32, today: &str) -> Vec<String> {
    let Some(today) = days_of(today) else { return vec![format!("bad date {today}")] };
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return errors };
    let mut process = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(d) = daily_date(&name).and_then(days_of) {
            if d < today - i64::from(keep_days) {
                if let Err(err) = std::fs::remove_file(e.path()) {
                    errors.push(format!("{name}: {err}"));
                }
            }
        } else if is_process_log(&name) {
            process.push((name, e.path()));
        }
    }
    // Each kind's stamp leads its name, so name order is start order.
    for p in PROCESS {
        let mut of_kind: Vec<_> = process.iter().filter(|(n, _)| n.starts_with(p)).collect();
        of_kind.sort();
        for (name, path) in of_kind.iter().rev().skip(KEEP_PROCESS_LOGS) {
            if let Err(err) = std::fs::remove_file(path) {
                errors.push(format!("{name}: {err}"));
            }
        }
    }
    errors
}

/// Every write, stderr included, passes through `config::redact`: the stream key never lands in a log.
#[derive(Clone)]
pub struct Logs {
    pub dir: PathBuf,
    /// Every key this process has seen: a line about the old key can still arrive after a reload.
    keys: Arc<Mutex<Vec<String>>>,
    /// Start time of this run, in the process log names.
    stamp: String,
}

impl Logs {
    pub fn new(dir: PathBuf, key: &str) -> Logs {
        let logs = Logs { dir, keys: Arc::default(), stamp: local_stamp() };
        logs.add_key(key);
        logs
    }
    pub fn add_key(&self, key: &str) {
        let mut keys = self.keys.lock().unwrap();
        if !key.is_empty() && !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    fn redact(&self, text: &str) -> String {
        config::redact(text, &self.keys.lock().unwrap())
    }
    pub fn append(&self, file: &str, text: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join(file)) {
            let _ = f.write_all(self.redact(text).as_bytes());
        }
    }
    /// A whole-file write (run.json, playlist.json).
    pub fn write(&self, file: &str, text: &str) {
        let _ = std::fs::write(self.dir.join(file), self.redact(text));
    }
    /// Append to today's file of a growing log.
    pub fn daily(&self, base: &str, ext: &str, text: &str) {
        self.append(&daily_name(base, ext, &local_date()), text);
    }
    /// Append to the log of one ffmpeg process (`kind` is "encoder" or "feeder", `n` its generation or seq).
    pub fn process(&self, kind: &str, n: u64, text: &str) {
        self.append(&format!("{kind}-{}-{n:06}.log", self.stamp), text);
    }
    pub fn csv(&self, base: &str, row: &[String]) {
        let clean: Vec<String> = row.iter().map(|c| c.replace([',', '\r', '\n'], " ")).collect();
        self.daily(base, "csv", &format!("{}\n", clean.join(",")));
    }
    pub fn event(&self, level: &str, kind: &str, extra: Value) {
        let mut v = json!({"ts": crate::iso_now(), "level": level, "kind": kind});
        if let (Some(m), Value::Object(e)) = (v.as_object_mut(), extra) {
            m.extend(e);
        }
        eprintln!("{level:8} {kind} {}", self.redact(&v.to_string()));
        self.daily("events", "jsonl", &format!("{v}\n"));
    }
    /// Prune old files; failures become one warn event and never stop the supervisor.
    pub fn prune(&self) {
        let errors = prune(&self.dir, KEEP_DAYS, &local_date());
        if let Some(first) = errors.first() {
            self.event("warn", "log-prune-failed", json!({"failed": errors.len(), "first": first}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sl-logs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn daily_name_puts_the_date_before_the_extension() {
        assert_eq!(daily_name("events", "jsonl", "2026-10-23"), "events-2026-10-23.jsonl");
    }

    #[test]
    fn prune_drops_daily_files_older_than_a_week_by_name_date() {
        let dir = temp("daily");
        for n in ["events-2026-10-15.jsonl", "events-2026-10-16.jsonl", "seams-2026-10-01.csv", "encoder-2026-10-22.csv", "ytdlp-2026-09-30.log", "events-2026-10-23.jsonl"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }

        let errors = prune(&dir, 7, "2026-10-23");

        let left = names(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(left, ["encoder-2026-10-22.csv", "events-2026-10-16.jsonl", "events-2026-10-23.jsonl"]);
    }

    #[test]
    fn prune_keeps_the_newest_ten_process_logs_of_each_kind() {
        let dir = temp("process");
        for i in 0..14 {
            std::fs::write(dir.join(format!("encoder-20261001-0000{i:02}-000001.log")), "x").unwrap();
            std::fs::write(dir.join(format!("feeder-20261001-0000{i:02}-{i:06}.log")), "x").unwrap();
        }

        prune(&dir, 7, "2026-10-23");

        let left = names(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(left.iter().filter(|n| n.starts_with("encoder-")).count(), 10);
        assert_eq!(left.iter().filter(|n| n.starts_with("feeder-")).count(), 10);
        assert!(left.contains(&"encoder-20261001-000013-000001.log".to_string()));
        assert!(!left.contains(&"encoder-20261001-000003-000001.log".to_string()));
    }

    #[test]
    fn prune_never_touches_state_run_or_unknown_files() {
        let dir = temp("unknown");
        let keep = ["state.json", "run.json", "playlist.json", "notes-2020-01-01.txt", "events-2020-13-45.jsonl", "mine-2020-01-01.csv", "events.jsonl", "encoder.log"];
        for n in keep {
            std::fs::write(dir.join(n), "x").unwrap();
        }

        prune(&dir, 7, "2026-10-23");

        let left = names(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        let mut want: Vec<String> = keep.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(left, want);
    }

    #[test]
    fn prune_counts_across_month_and_year_ends() {
        let dir = temp("calendar");
        for n in ["events-2026-12-24.jsonl", "events-2026-12-25.jsonl", "events-2025-12-31.jsonl"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }

        prune(&dir, 7, "2027-01-01");

        let left = names(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(left, ["events-2026-12-25.jsonl"]);
    }

    #[test]
    fn daily_writes_land_in_todays_file_and_process_writes_in_their_own() {
        let dir = temp("write");
        let logs = Logs::new(dir.clone(), "live_1_SeCrEt");

        logs.csv("seams", &["a,b".into(), "live_1_SeCrEt".into()]);
        logs.process("feeder", 7, "key live_1_SeCrEt\n");

        let left = names(&dir);
        let feeder_name = left.iter().find(|n| n.starts_with("feeder-")).unwrap().clone();
        let seams = std::fs::read_to_string(dir.join(daily_name("seams", "csv", &local_date()))).unwrap();
        let feeder = std::fs::read_to_string(dir.join(&feeder_name)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seams, "a b,***\n");
        assert_eq!(feeder, "key ***\n");
        assert!(is_process_log(&feeder_name), "{feeder_name}");
    }

    #[test]
    fn logs_redact_a_key_added_later_and_every_earlier_one() {
        let dir = temp("keys");
        let logs = Logs::new(dir.clone(), "");

        logs.append("a.log", "before live_OLD_1\n");
        logs.add_key("live_OLD_1");
        logs.add_key("live_NEW_2");
        logs.append("a.log", "old live_OLD_1 new live_NEW_2\n");

        let written = std::fs::read_to_string(dir.join("a.log")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(written, "before live_OLD_1\nold *** new ***\n");
    }
}
