//! The bundled yt-dlp's self-update: `-U`, then `--version` for the result.

use std::path::Path;
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;
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
