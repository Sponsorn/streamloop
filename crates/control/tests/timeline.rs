use std::sync::Arc;
use std::time::{Duration, SystemTime};

use streamloop_common::time::local_date;
use streamloop_control::clock::{Clock, SystemClock, TestClock};
use streamloop_control::timeline::{EventLog, EventStore, TimelineEntry};

fn entry(t: &str, m: &str) -> TimelineEntry {
    TimelineEntry { timestamp: t.into(), message: m.into() }
}

fn messages(v: &[TimelineEntry]) -> Vec<&str> {
    v.iter().map(|e| e.message.as_str()).collect()
}

fn store(dir: &std::path::Path) -> EventStore {
    EventStore::new(dir, Arc::new(SystemClock))
}

#[tokio::test]
async fn appends_are_read_back_oldest_first() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    // Act
    s.append(&entry("2026-06-09T00:00:01Z", "first"));
    s.append(&entry("2026-06-09T00:00:02Z", "second"));
    s.flush().await;
    // Assert
    assert_eq!(messages(&s.load_recent(10)), ["first", "second"]);
}

#[tokio::test]
async fn writes_to_a_date_named_jsonl_file() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    // Act
    s.append(&entry("t", "x"));
    s.flush().await;
    // Assert
    let name = format!("events-{}.jsonl", local_date(SystemTime::now()));
    assert!(dir.path().join(name).exists());
}

#[tokio::test]
async fn load_recent_returns_at_most_limit_of_the_most_recent() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    for i in 1..=5 {
        s.append(&entry(&format!("t{i}"), &format!("m{i}")));
    }
    s.flush().await;
    // Act
    let got = s.load_recent(2);
    // Assert
    assert_eq!(messages(&got), ["m4", "m5"]);
}

#[test]
fn construction_deletes_timeline_files_older_than_seven_days() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("events-2000-01-01.jsonl");
    let other = dir.path().join("supervisor-events-2000-01-01.jsonl");
    std::fs::write(&old, "{\"timestamp\":\"t\",\"message\":\"old\"}\n").unwrap();
    std::fs::write(&other, "x").unwrap();
    let clock = TestClock::new(SystemTime::now() + Duration::from_secs(30 * 24 * 3600));
    // Act
    let _ = EventStore::new(dir.path(), Arc::new(clock));
    // Assert
    assert!(!old.exists());
    assert!(other.exists());
}

#[test]
fn construction_keeps_a_recent_timeline_file() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let recent = dir.path().join("events-2026-11-08.jsonl");
    std::fs::write(&recent, "{\"timestamp\":\"t\",\"message\":\"new\"}
").unwrap();
    // Act
    let _ = EventStore::new(dir.path(), Arc::new(TestClock::new(SystemTime::now())));
    // Assert
    assert!(recent.exists());
}

#[test]
fn load_recent_is_empty_when_no_files_exist() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    // Act
    let got = store(dir.path()).load_recent(10);
    // Assert
    assert!(got.is_empty());
}

#[tokio::test]
async fn keeps_only_the_last_100_events() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::new(store(dir.path()));
    // Act
    for i in 0..105 {
        log.add(&format!("e{i}"));
    }
    let got = log.entries();
    log.flush().await;
    // Assert
    assert_eq!(got.len(), 100);
    assert_eq!(got[0].message, "e5");
    assert_eq!(got[99].message, "e104");
}

#[tokio::test]
async fn reloads_persisted_events_in_a_new_instance() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::new(store(dir.path()));
    log.add("first");
    log.add("second");
    log.flush().await;
    // Act
    let again = EventLog::new(store(dir.path()));
    // Assert
    assert_eq!(messages(&again.entries()), ["first", "second"]);
}

#[tokio::test]
async fn loads_a_timeline_file_written_by_node() {
    // Arrange: lines as event-store.ts writes them (JSON.stringify of {timestamp, message}), one corrupt, one partial
    let dir = tempfile::tempdir().unwrap();
    let name = format!("events-{}.jsonl", local_date(SystemTime::now()));
    let text = concat!(
        "{\"timestamp\":\"2026-11-08T10:15:30.123Z\",\"message\":\"Stream started\"}\n",
        "not json\n",
        "\n",
        "{\"timestamp\":\"2026-11-08T10:16:00.000Z\",\"message\":\"Q\\\"uote \u{00e5}\"}\n",
        "{\"timestamp\":\"2026-11-08T10:17:00.000Z\",\"mess",
    );
    std::fs::write(dir.path().join(name), text).unwrap();
    // Act
    let got = store(dir.path()).load_recent(10);
    // Assert
    assert_eq!(got, [entry("2026-11-08T10:15:30.123Z", "Stream started"), entry("2026-11-08T10:16:00.000Z", "Q\"uote \u{00e5}")]);
}

#[tokio::test]
async fn writes_lines_in_the_shape_node_writes() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(SystemTime::UNIX_EPOCH + Duration::from_millis(1_792_000_000_123)));
    let log = EventLog::new(EventStore::new(dir.path(), clock.clone() as Arc<dyn Clock>));
    // Act
    log.add("Stream started");
    log.flush().await;
    // Assert
    let name = format!("events-{}.jsonl", local_date(clock.now()));
    let text = std::fs::read_to_string(dir.path().join(name)).unwrap();
    assert_eq!(text, "{\"timestamp\":\"2026-10-14T17:46:40.123Z\",\"message\":\"Stream started\"}\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn appends_keep_call_order_under_load() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(EventLog::new(store(dir.path())));
    // Act
    let tasks: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|who| {
            let log = log.clone();
            tokio::spawn(async move {
                for i in 0..500 {
                    log.add(&format!("{who}{i}"));
                    if i % 50 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    log.flush().await;
    // Assert: the in-memory window and the file each hold a task's entries in its own order
    let in_file = store(dir.path()).load_recent(2000);
    assert_eq!(in_file.len(), 1000);
    for who in ["a", "b"] {
        let order: Vec<usize> = in_file.iter().filter_map(|e| e.message.strip_prefix(who)?.parse().ok()).collect();
        assert_eq!(order, (0..500).collect::<Vec<_>>(), "file order for {who}");
        let mem: Vec<usize> = log.entries().iter().filter_map(|e| e.message.strip_prefix(who)?.parse().ok()).collect();
        assert!(mem.windows(2).all(|w| w[0] + 1 == w[1]), "memory order for {who}");
    }
}
