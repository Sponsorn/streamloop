use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::{json, Map, Value};
use streamloop_control::logging::{cleanup_old_files, format_line, format_line_ordered, LocalTime};
use streamloop_common::config::ordered::Value as Ordered;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn t1() -> LocalTime {
    LocalTime { year: 2025, month: 2, day: 16, hour: 14, minute: 3, second: 5, millisecond: 123 }
}

fn extras(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn touch(dir: &Path, name: &str, age: Duration, now: SystemTime) {
    let file = File::create(dir.join(name)).unwrap();
    file.set_modified(now - age).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn formats_a_basic_info_line_with_a_local_timestamp() {
    // Act
    let line = format_line(30, "Server listening on port 7654", None, t1());
    // Assert
    assert_eq!(line, "[2025-02-16 14:03:05.123] INFO: Server listening on port 7654");
}

#[test]
fn appends_extra_fields_as_json() {
    // Arrange
    let e = extras(json!({ "port": 7654 }));
    // Act
    let line = format_line(30, "Server listening", Some(&e), t1());
    // Assert
    assert_eq!(line, "[2025-02-16 14:03:05.123] INFO: Server listening {\"port\":7654}");
}

#[test]
fn formats_the_warn_level_with_its_extras() {
    // Arrange
    let e = extras(json!({ "stalledHeartbeats": 2, "videoIndex": 5 }));
    // Act
    let line = format_line(40, "Player stalled", Some(&e), t1());
    // Assert
    assert!(line.contains("WARN: Player stalled"));
    assert!(line.contains("\"stalledHeartbeats\":2"));
    assert!(line.contains("\"videoIndex\":5"));
}

#[test]
fn formats_the_error_level_with_its_extras() {
    // Arrange
    let e = extras(json!({ "status": 429 }));
    // Act
    let line = format_line(50, "Discord webhook failed", Some(&e), t1());
    // Assert
    assert!(line.contains("ERROR: Discord webhook failed"));
    assert!(line.contains("\"status\":429"));
}

#[test]
fn an_empty_message_leaves_the_text_after_the_level_empty() {
    // Act
    let line = format_line(30, "", None, t1());
    // Assert
    assert_eq!(line, "[2025-02-16 14:03:05.123] INFO: ");
}

#[test]
fn an_unknown_level_number_is_named_lvl_and_the_number() {
    // Act
    let line = format_line(25, "custom level", None, t1());
    // Assert
    assert!(line.contains("LVL25: custom level"));
}

#[test]
fn deletes_log_files_older_than_the_retention_period() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    touch(dir.path(), "streamloop-2025-01-01.log", 30 * DAY, now);
    touch(dir.path(), "streamloop-2025-02-15.log", Duration::ZERO, now);
    // Act
    cleanup_old_files(dir.path(), "streamloop-", 7, now);
    // Assert
    assert_eq!(names(dir.path()), vec!["streamloop-2025-02-15.log"]);
}

#[test]
fn keeps_log_files_within_the_retention_period() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    touch(dir.path(), "streamloop-2025-02-15.log", 6 * DAY, now);
    // Act
    cleanup_old_files(dir.path(), "streamloop-", 7, now);
    // Assert
    assert_eq!(names(dir.path()), vec!["streamloop-2025-02-15.log"]);
}

#[test]
fn leaves_files_that_do_not_match_the_prefix_and_extension() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    touch(dir.path(), "other-file.txt", 30 * DAY, now);
    touch(dir.path(), "streamloop-2025-01-01.txt", 30 * DAY, now);
    // Act
    cleanup_old_files(dir.path(), "streamloop-", 7, now);
    // Assert
    assert_eq!(names(dir.path()), vec!["other-file.txt", "streamloop-2025-01-01.txt"]);
}

#[test]
fn a_missing_directory_is_not_an_error() {
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    // Act
    cleanup_old_files(&dir.path().join("nonexistent"), "streamloop-", 7, SystemTime::now());
    // Assert: reaching this line without a panic is the check
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureTime {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millisecond: u32,
}

#[derive(Deserialize)]
struct FixtureInput {
    #[serde(rename = "localTime")]
    local_time: FixtureTime,
    fields: Ordered,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    input: FixtureInput,
    output: String,
}

#[test]
fn matches_every_log_line_fixture() {
    // Arrange
    let raw = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/log-lines.json")).unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let t = &case.input.local_time;
        let local = LocalTime { year: t.year, month: t.month, day: t.day, hour: t.hour, minute: t.minute, second: t.second, millisecond: t.millisecond };
        let mut level = 0u64;
        let mut msg = String::new();
        let mut rest: Vec<(String, Ordered)> = Vec::new();
        let Ordered::Object(fields) = case.input.fields else { panic!("fields is an object") };
        for (k, v) in fields {
            match k.as_str() {
                "level" => level = if let Ordered::Number(n) = v { n as u64 } else { panic!("level") },
                "msg" => msg = if let Ordered::String(s) = v { s } else { panic!("msg") },
                "time" | "pid" | "hostname" | "v" => {}
                _ => rest.push((k, v)),
            }
        }
        // Act
        let line = format_line_ordered(level as u8, &msg, &rest, local);
        // Assert
        assert_eq!(line, case.output, "{}", case.name);
    }
}
