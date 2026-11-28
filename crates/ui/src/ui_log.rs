//! `ui.log`, the app's own log: its panics, a refused argument and a tray icon that could not be made.

use std::{fs, io::Write, path::Path};

pub const FILE: &str = "ui.log";
const ROLL_PAST: u64 = 1024 * 1024;

/// Appends `line` to `ui.log` in `dir`, first moving a log past 1 MB to `ui.log.old`.
pub fn append(dir: &Path, line: &str) {
    let _ = fs::create_dir_all(dir);
    let path = dir.join(FILE);
    if fs::metadata(&path).is_ok_and(|m| m.len() > ROLL_PAST) {
        let _ = fs::rename(&path, dir.join(format!("{FILE}.old")));
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// `line` after the local time.
pub fn stamped(line: &str) -> String {
    format!("{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_the_log_to_old_past_one_megabyte() {
        // Arrange
        let dir = std::env::temp_dir().join(format!("streamloop-ui-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("ui.log");
        let old = dir.join("ui.log.old");
        append(&dir, "first");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "first\n");
        assert!(!old.exists());
        std::fs::write(&log, vec![b'x'; 1024 * 1024 + 1]).unwrap();

        // Act
        append(&dir, "second");

        // Assert
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "second\n");
        assert_eq!(std::fs::metadata(&old).unwrap().len(), 1024 * 1024 + 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
