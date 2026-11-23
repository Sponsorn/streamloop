//! The bundled yt-dlp's self-update: `-U`, then `--version` for the result.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::runner::{RunOutput, RunSpec, Runner};

const UPDATE_TIMEOUT: Duration = Duration::from_secs(120);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YtdlpUpdate {
    pub ok: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

impl YtdlpUpdate {
    fn failed(error: impl Into<String>) -> Self {
        Self { ok: false, version: None, error: Some(error.into()) }
    }
}

/// A run that ended and exited 0, or the reason it did not.
pub(crate) async fn run_checked(runner: &dyn Runner, spec: RunSpec) -> Result<RunOutput, String> {
    let out = runner.run(spec).await.map_err(|e| e.message)?;
    match out.code {
        Some(0) => Ok(out),
        code => {
            let stderr = out.stderr_text();
            let detail = stderr.trim();
            let status = code.map_or("yt-dlp ended without an exit code".to_string(), |c| format!("yt-dlp exited with code {c}"));
            Err(if detail.is_empty() { status } else { format!("{status}: {detail}") })
        }
    }
}

/// Self-update only works on the standalone binary in the install. In dev the file may be missing;
/// that returns `ok: false` instead of an error, so the caller can run it at start unguarded.
pub async fn update_ytdlp(runner: &dyn Runner, ytdlp: &Path, exists: impl Fn(&Path) -> bool) -> YtdlpUpdate {
    if !exists(ytdlp) {
        return YtdlpUpdate::failed("yt-dlp binary not found (dev mode or missing install)");
    }
    let program = ytdlp.to_string_lossy();
    if let Err(e) = run_checked(runner, RunSpec::new(&program, &["-U"], UPDATE_TIMEOUT)).await {
        return YtdlpUpdate::failed(e);
    }
    match run_checked(runner, RunSpec::new(&program, &["--version"], VERSION_TIMEOUT)).await {
        Ok(out) => YtdlpUpdate { ok: true, version: Some(out.stdout_text().trim().to_string()), error: None },
        Err(e) => YtdlpUpdate::failed(e),
    }
}

/// While YouTube lookups are paused, at most one update this often.
pub const PAUSED_UPDATE_EVERY: Duration = Duration::from_secs(3600);

/// A new YouTube player can break every lookup until yt-dlp catches up, often within hours; the update at
/// start alone would leave the stream on the slate until the next restart.
#[derive(Debug, Default)]
pub struct PausedUpdates {
    last: Option<SystemTime>,
}

impl PausedUpdates {
    /// True for the pause opening or a failed probe, once an hour; a due update counts as run.
    pub fn due(&mut self, kind: &str, now: SystemTime) -> bool {
        if kind != "youtube-breaker-open" && kind != "youtube-probe-failed" {
            return false;
        }
        if self.last.is_some_and(|last| now.duration_since(last).is_ok_and(|d| d < PAUSED_UPDATE_EVERY)) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;
    use crate::runner::{FakeRunner, RunError, RunErrorKind};

    #[tokio::test]
    async fn skips_without_running_anything_when_the_binary_does_not_exist() {
        // Arrange
        let runner = FakeRunner::new();
        // Act
        let result = update_ytdlp(&runner, Path::new("/missing/yt-dlp.exe"), |_| false).await;
        // Assert
        assert!(!result.ok);
        assert!(result.error.unwrap().to_lowercase().contains("not found"));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn runs_update_then_version_and_returns_the_trimmed_version() {
        // Arrange
        let runner = FakeRunner::new();
        runner.push(Ok(RunOutput::ok("Updated yt-dlp to stable@2026.06.01")));
        runner.push(Ok(RunOutput::ok("2026.06.01\n")));
        // Act
        let result = update_ytdlp(&runner, Path::new("/bin/yt-dlp.exe"), |_| true).await;
        // Assert
        assert_eq!(result, YtdlpUpdate { ok: true, version: Some("2026.06.01".into()), error: None });
        let calls = runner.calls();
        assert_eq!(calls[0].args, ["-U"]);
        assert_eq!(calls[1].args, ["--version"]);
        assert_eq!((calls[0].timeout, calls[1].timeout), (Duration::from_secs(120), Duration::from_secs(10)));
    }

    fn at(s: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(s)
    }

    #[test]
    fn an_update_is_due_when_youtube_lookups_pause() {
        // Arrange
        let mut updates = PausedUpdates::default();
        // Act
        let due = updates.due("youtube-breaker-open", at(1_000));
        // Assert
        assert!(due);
    }

    #[test]
    fn no_second_update_is_due_within_the_hour() {
        // Arrange
        let mut updates = PausedUpdates::default();
        updates.due("youtube-breaker-open", at(1_000));
        // Act
        let due = updates.due("youtube-probe-failed", at(1_000 + 3_599));
        // Assert
        assert!(!due);
    }

    #[test]
    fn a_failed_probe_an_hour_later_is_due_again() {
        // Arrange
        let mut updates = PausedUpdates::default();
        updates.due("youtube-breaker-open", at(1_000));
        // Act
        let due = updates.due("youtube-probe-failed", at(1_000 + 3_600));
        // Assert
        assert!(due);
    }

    #[test]
    fn other_events_never_make_an_update_due() {
        // Arrange
        let mut updates = PausedUpdates::default();
        // Act
        let due: Vec<bool> = ["youtube-breaker-closed", "youtube-probe", "resolve-failed", "download-failed"].iter().map(|k| updates.due(k, at(1_000))).collect();
        // Assert
        assert_eq!(due, [false; 4]);
    }

    #[tokio::test]
    async fn reports_the_error_message_when_the_update_fails() {
        // Arrange
        let runner = FakeRunner::new();
        runner.push(Err(RunError { kind: RunErrorKind::Wait, message: "network unreachable".into() }));
        // Act
        let result = update_ytdlp(&runner, Path::new("/bin/yt-dlp.exe"), |_| true).await;
        // Assert
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("network unreachable"));
        assert_eq!(runner.calls().len(), 1);
    }
}
