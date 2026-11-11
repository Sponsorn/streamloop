//! Where the supervisor's events log sits, shared by the supervisor that writes it and the control process that tails it.

use std::path::{Path, PathBuf};

/// Since 3.0.5; named apart from the control process's own `events-DATE.jsonl` so both fit in one folder.
pub const SUPERVISOR_EVENTS: &str = "supervisor-events";
/// The events log's name up to 3.0.4.
pub const LEGACY_EVENTS: &str = "events";

/// The events file of `date` (YYYY-MM-DD): `supervisor-events-DATE.jsonl` when it exists, else `events-DATE.jsonl`.
pub fn events_file(logs_dir: &Path, date: &str) -> PathBuf {
    let current = logs_dir.join(format!("{SUPERVISOR_EVENTS}-{date}.jsonl"));
    if current.exists() {
        current
    } else {
        logs_dir.join(format!("{LEGACY_EVENTS}-{date}.jsonl"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sl-common-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn events_file_prefers_the_supervisor_events_name() {
        // Arrange
        let dir = scratch("both");
        std::fs::write(dir.join("events-2026-11-08.jsonl"), "").unwrap();
        std::fs::write(dir.join("supervisor-events-2026-11-08.jsonl"), "").unwrap();
        // Act
        let f = events_file(&dir, "2026-11-08");
        // Assert
        assert_eq!(f, dir.join("supervisor-events-2026-11-08.jsonl"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn events_file_falls_back_to_the_name_up_to_3_0_4() {
        // Arrange
        let dir = scratch("legacy");
        std::fs::write(dir.join("events-2026-11-08.jsonl"), "").unwrap();
        // Act
        let f = events_file(&dir, "2026-11-08");
        // Assert
        assert_eq!(f, dir.join("events-2026-11-08.jsonl"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
