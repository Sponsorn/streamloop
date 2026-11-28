//! The bundled yt-dlp's self-update: `-U` on a copy, then `--version` for the result.

use std::path::Path;
use std::time::{Duration, SystemTime};

use streamloop_common::paths;
use streamloop_common::ytdlp_temp::RunTemp;

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

/// `run_checked` for yt-dlp, which unpacks into a folder of its own beside `base`, gone when the run ends.
pub(crate) async fn run_ytdlp(runner: &dyn Runner, base: &Path, mut spec: RunSpec) -> Result<RunOutput, String> {
    let temp = RunTemp::new(base, "control");
    if let Some(temp) = &temp {
        spec.env.extend(temp.env());
    }
    run_checked(runner, spec).await
}

/// Copies past the newest two are removed: a run may have picked the second newest just before the newest appeared.
const COPIES_KEPT: usize = 2;

/// One update at a time, since they share the work copy.
static UPDATING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Updates a copy of the yt-dlp runs use and keeps it as `yt-dlp-<version>.exe` beside `base`, the shipped exe.
/// A missing exe (dev) is `ok: false`, not an error, so the start can run it unguarded.
pub async fn update_ytdlp(runner: &dyn Runner, base: &Path) -> YtdlpUpdate {
    let _one = UPDATING.lock().await;
    let current = paths::ytdlp_to_run(base);
    if !current.exists() {
        return YtdlpUpdate::failed("yt-dlp binary not found (dev mode or missing install)");
    }
    let work = paths::ytdlp_update_copy(base);
    if let Err(e) = tokio::fs::copy(&current, &work).await {
        return YtdlpUpdate::failed(format!("Could not copy {} to update it: {e}", current.display()));
    }
    let result = update_copy(runner, base, &work).await;
    // Gone already when the update was kept.
    let _ = tokio::fs::remove_file(&work).await;
    for old in paths::ytdlp_copies(base).into_iter().skip(COPIES_KEPT) {
        // Windows refuses to delete an exe that runs, so a copy in use stays for a later update.
        let _ = tokio::fs::remove_file(&old).await;
    }
    result
}

async fn update_copy(runner: &dyn Runner, base: &Path, work: &Path) -> YtdlpUpdate {
    let program = work.to_string_lossy();
    if let Err(e) = run_ytdlp(runner, base, RunSpec::new(&program, &["-U"], UPDATE_TIMEOUT)).await {
        return YtdlpUpdate::failed(e);
    }
    let version = match run_ytdlp(runner, base, RunSpec::new(&program, &["--version"], VERSION_TIMEOUT)).await {
        Ok(out) => out.stdout_text().trim().to_string(),
        Err(e) => return YtdlpUpdate::failed(e),
    };
    let Some(copy) = paths::ytdlp_copy(base, &version) else {
        return YtdlpUpdate::failed(format!("yt-dlp printed an unexpected version: {version}"));
    };
    if !copy.exists() {
        if let Err(e) = tokio::fs::rename(work, &copy).await {
            return YtdlpUpdate::failed(format!("Could not keep the updated yt-dlp as {}: {e}", copy.display()));
        }
    }
    YtdlpUpdate { ok: true, version: Some(version), error: None }
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
    use std::path::PathBuf;
    use crate::runner::{FakeRunner, RunError, RunErrorKind, RunOutput};

    /// An install's `yt-dlp\` with `files`, each holding its own name; returns the shipped exe's path.
    fn install(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            std::fs::write(dir.path().join(file), file).unwrap();
        }
        let base = dir.path().join("yt-dlp.exe");
        (dir, base)
    }

    fn names(dir: &tempfile::TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn updated_to(runner: &FakeRunner, version: &str) {
        runner.push(Ok(RunOutput::ok(&format!("Updated yt-dlp to nightly@{version}"))));
        runner.push(Ok(RunOutput::ok(&format!("{version}\n"))));
    }

    #[tokio::test]
    async fn skips_without_running_anything_when_the_binary_does_not_exist() {
        // Arrange
        let runner = FakeRunner::new();
        // Act
        let result = update_ytdlp(&runner, Path::new("/missing/yt-dlp.exe")).await;
        // Assert
        assert!(!result.ok);
        assert!(result.error.unwrap().to_lowercase().contains("not found"));
        assert!(runner.calls().is_empty());
    }

    #[tokio::test]
    async fn the_update_runs_on_a_copy_and_never_on_the_exe_runs_use() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe", "deno.exe"]);
        let runner = FakeRunner::new();
        updated_to(&runner, "2026.11.24.232653");
        // Act
        let result = update_ytdlp(&runner, &base).await;
        // Assert
        assert_eq!(result, YtdlpUpdate { ok: true, version: Some("2026.11.24.232653".into()), error: None });
        let calls = runner.calls();
        let work = dir.path().join("yt-dlp-update.exe").to_string_lossy().into_owned();
        assert_eq!((calls[0].program.as_str(), calls[0].args.as_slice()), (work.as_str(), ["-U".to_string()].as_slice()));
        assert_eq!((calls[1].program.as_str(), calls[1].args.as_slice()), (work.as_str(), ["--version".to_string()].as_slice()));
        assert_eq!((calls[0].timeout, calls[1].timeout), (Duration::from_secs(120), Duration::from_secs(10)));
        assert_eq!(names(&dir), ["deno.exe", "yt-dlp-2026.11.24.232653.exe", "yt-dlp.exe"]);
        assert_eq!(std::fs::read_to_string(&base).unwrap(), "yt-dlp.exe", "the shipped exe is never changed");
    }

    #[tokio::test]
    async fn the_next_update_starts_from_the_newest_copy() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe", "yt-dlp-2026.11.20.exe", "yt-dlp-2026.11.3.exe"]);
        let runner = FakeRunner::new();
        updated_to(&runner, "2026.11.24");
        // Act
        update_ytdlp(&runner, &base).await;
        // Assert
        assert_eq!(std::fs::read_to_string(dir.path().join("yt-dlp-2026.11.24.exe")).unwrap(), "yt-dlp-2026.11.20.exe");
    }

    #[tokio::test]
    async fn an_update_that_finds_nothing_newer_leaves_the_copy_as_it_was() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe", "yt-dlp-2026.11.24.exe"]);
        let runner = FakeRunner::new();
        runner.push(Ok(RunOutput::ok("yt-dlp is up to date (nightly@2026.11.24)")));
        runner.push(Ok(RunOutput::ok("2026.11.24\n")));
        // Act
        let result = update_ytdlp(&runner, &base).await;
        // Assert
        assert!(result.ok);
        assert_eq!(names(&dir), ["yt-dlp-2026.11.24.exe", "yt-dlp.exe"]);
        assert_eq!(std::fs::read_to_string(dir.path().join("yt-dlp-2026.11.24.exe")).unwrap(), "yt-dlp-2026.11.24.exe");
    }

    #[tokio::test]
    async fn an_update_keeps_the_two_newest_copies_and_the_shipped_exe() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe", "yt-dlp-2026.11.20.exe", "yt-dlp-2026.11.22.exe", "yt-dlp-2026.11.23.exe"]);
        let runner = FakeRunner::new();
        updated_to(&runner, "2026.11.24");
        // Act
        update_ytdlp(&runner, &base).await;
        // Assert
        assert_eq!(names(&dir), ["yt-dlp-2026.11.23.exe", "yt-dlp-2026.11.24.exe", "yt-dlp.exe"]);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_copy_still_running_is_left_for_a_later_update() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe", "yt-dlp-2026.11.22.exe", "yt-dlp-2026.11.23.exe"]);
        let running = dir.path().join("yt-dlp-2026.11.1.exe");
        std::fs::copy(r"C:\Windows\System32\PING.EXE", &running).unwrap();
        let mut child = streamloop_common::procs::command(&running).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let runner = FakeRunner::new();
        updated_to(&runner, "2026.11.24");
        // Act
        update_ytdlp(&runner, &base).await;
        let left = names(&dir);
        child.kill().unwrap();
        child.wait().unwrap();
        // Assert
        assert_eq!(left, ["yt-dlp-2026.11.1.exe", "yt-dlp-2026.11.23.exe", "yt-dlp-2026.11.24.exe", "yt-dlp.exe"]);
    }

    #[tokio::test]
    async fn a_version_that_is_not_digits_and_dots_makes_no_copy() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe"]);
        let runner = FakeRunner::new();
        runner.push(Ok(RunOutput::ok("Updated")));
        runner.push(Ok(RunOutput::ok("nightly@2026.11.24\n")));
        // Act
        let result = update_ytdlp(&runner, &base).await;
        // Assert
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("nightly@2026.11.24"));
        assert_eq!(names(&dir), ["yt-dlp.exe"]);
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
    async fn each_update_run_unpacks_into_its_own_temp_folder_and_leaves_none() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe"]);
        let runner = FakeRunner::new();
        updated_to(&runner, "2026.11.24");
        // Act
        update_ytdlp(&runner, &base).await;
        // Assert
        let temp = dir.path().join("temp");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        for call in calls {
            let tmp: Vec<&str> = ["TMP", "TEMP"].iter().filter_map(|k| call.env.iter().find(|(name, _)| name == k).map(|(_, v)| v.as_str())).collect();
            assert!(tmp.len() == 2 && tmp[0] == tmp[1] && Path::new(tmp[0]).starts_with(&temp), "{:?}", call.env);
        }
        assert!(!temp.exists(), "the runs' temp folders were left behind");
    }

    #[tokio::test]
    async fn a_failed_update_reports_why_and_removes_its_work_copy() {
        // Arrange
        let (dir, base) = install(&["yt-dlp.exe"]);
        let runner = FakeRunner::new();
        runner.push(Err(RunError { kind: RunErrorKind::Wait, message: "network unreachable".into() }));
        // Act
        let result = update_ytdlp(&runner, &base).await;
        // Assert
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("network unreachable"));
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(names(&dir), ["yt-dlp.exe"]);
    }
}
