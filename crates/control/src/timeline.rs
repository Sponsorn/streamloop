//! The dashboard timeline: daily JSONL files in `app\logs\` and the last 100 entries in memory.
//! The files are compatible both ways with 3.0.x's Node: same names, same line shape.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use streamloop_common::time::{iso, local_date};

use crate::clock::Clock;

const FILE_PREFIX: &str = "events-";
const RETENTION_DAYS: u64 = 7;
const MAX_ENTRIES: usize = 100;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// One timeline line. Field order is the key order Node writes (`timestamp`, then `message`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp: String,
    pub message: String,
}

enum Job {
    Write { path: PathBuf, line: String },
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// Daily-rotated JSONL files. Appends go to one writer thread, so they never block the caller and land in call order.
pub struct EventStore {
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    tx: Mutex<Sender<Job>>,
}

impl EventStore {
    /// Creates `dir` and deletes `events-*.jsonl` files last written more than 7 days ago.
    pub fn new(dir: &Path, clock: Arc<dyn Clock>) -> EventStore {
        let _ = fs::create_dir_all(dir);
        cleanup_old_files(dir, clock.now());
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in rx {
                match job {
                    Job::Write { path, line } => {
                        let result = OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(line.as_bytes()));
                        if let Err(err) = result {
                            tracing::warn!(%err, "Failed to persist event");
                        }
                    }
                    Job::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        EventStore { dir: dir.to_path_buf(), clock, tx: Mutex::new(tx) }
    }

    pub(crate) fn now(&self) -> SystemTime {
        self.clock.now()
    }

    fn file_for(&self, date: &str) -> PathBuf {
        self.dir.join(format!("{FILE_PREFIX}{date}.jsonl"))
    }

    /// Queues one line for today's file (local date at the call). Failures are logged, never returned.
    pub fn append(&self, e: &TimelineEntry) {
        let Ok(mut line) = serde_json::to_string(e) else { return };
        line.push('\n');
        let path = self.file_for(&local_date(self.clock.now()));
        let _ = self.tx.lock().unwrap().send(Job::Write { path, line });
    }

    /// Resolves once every append queued before the call is on disk.
    pub async fn flush(&self) {
        let (done, wait) = tokio::sync::oneshot::channel();
        if self.tx.lock().unwrap().send(Job::Flush(done)).is_ok() {
            let _ = wait.await;
        }
    }

    /// Up to `limit` most recent entries from yesterday's and today's files, oldest first. Lines that do not parse are skipped.
    pub fn load_recent(&self, limit: usize) -> Vec<TimelineEntry> {
        let now = self.clock.now();
        let mut entries = Vec::new();
        for date in [local_date(now - DAY), local_date(now)] {
            let path = self.file_for(&date);
            let Ok(text) = fs::read(&path) else { continue };
            for raw in String::from_utf8_lossy(&text).split('\n') {
                if raw.is_empty() {
                    continue;
                }
                if let Ok(e) = serde_json::from_str::<TimelineEntry>(raw) {
                    entries.push(e);
                }
            }
        }
        let skip = entries.len().saturating_sub(limit);
        entries.split_off(skip)
    }
}

fn cleanup_old_files(dir: &Path, now: SystemTime) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let Some(cutoff) = now.checked_sub(DAY * RETENTION_DAYS as u32) else { return };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(FILE_PREFIX) || !name.ends_with(".jsonl") {
            continue;
        }
        if entry.metadata().and_then(|m| m.modified()).is_ok_and(|m| m < cutoff) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The last 100 events, persisted through the store.
pub struct EventLog {
    store: EventStore,
    entries: Mutex<VecDeque<TimelineEntry>>,
}

impl EventLog {
    pub fn new(store: EventStore) -> EventLog {
        let entries = store.load_recent(MAX_ENTRIES).into();
        EventLog { store, entries: Mutex::new(entries) }
    }

    pub fn add(&self, message: &str) {
        let ms = self.store.now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let entry = TimelineEntry { timestamp: iso(ms), message: message.to_string() };
        let mut entries = self.entries.lock().unwrap();
        entries.push_back(entry.clone());
        if entries.len() > MAX_ENTRIES {
            entries.pop_front();
        }
        // Queued under the lock so the file order is the in-memory order.
        self.store.append(&entry);
    }

    pub fn entries(&self) -> Vec<TimelineEntry> {
        self.entries.lock().unwrap().iter().cloned().collect()
    }

    pub async fn flush(&self) {
        self.store.flush().await;
    }
}
