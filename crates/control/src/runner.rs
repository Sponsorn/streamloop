//! Child processes behind a trait: yt-dlp, ffmpeg and `--version` checks run through it, and tests answer from `FakeRunner`.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

impl RunSpec {
    pub fn new(program: &str, args: &[&str], timeout: Duration) -> Self {
        Self { program: program.into(), args: args.iter().map(|a| a.to_string()).collect(), stdin: None, timeout, env: Vec::new() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunOutput {
    /// `None` when the process ended without an exit code.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    pub fn ok(stdout: &str) -> Self {
        Self { code: Some(0), stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() }
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunErrorKind {
    /// The program could not be started.
    Spawn,
    /// The process outlived its timeout and was killed.
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    pub kind: RunErrorKind,
    pub message: String,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RunError {}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, spec: RunSpec) -> Result<RunOutput, RunError>;
}

pub struct ProcessRunner;

#[async_trait]
impl Runner for ProcessRunner {
    async fn run(&self, spec: RunSpec) -> Result<RunOutput, RunError> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;

        let mut std_cmd = streamloop_common::procs::command(&spec.program);
        std_cmd.args(&spec.args).envs(spec.env.iter().map(|(k, v)| (k, v)));
        let mut cmd = tokio::process::Command::from(std_cmd);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        if spec.stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().map_err(|e| RunError { kind: RunErrorKind::Spawn, message: format!("{}: {e}", spec.program) })?;
        if let (Some(data), Some(mut stdin)) = (spec.stdin, child.stdin.take()) {
            // A child that exits without reading closes the pipe; that error is not the caller's.
            tokio::spawn(async move {
                let _ = stdin.write_all(&data).await;
            });
        }
        // Dropping the wait on timeout drops the child, and kill_on_drop ends it.
        match tokio::time::timeout(spec.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => Ok(RunOutput { code: out.status.code(), stdout: out.stdout, stderr: out.stderr }),
            Ok(Err(e)) => Err(RunError { kind: RunErrorKind::Spawn, message: format!("{}: {e}", spec.program) }),
            Err(_) => Err(RunError { kind: RunErrorKind::Timeout, message: format!("{} timed out after {} s", spec.program, spec.timeout.as_secs()) }),
        }
    }
}

/// Returns queued outputs in order and records every call. An empty queue is a `Spawn` error.
#[derive(Default)]
pub struct FakeRunner {
    queue: Mutex<VecDeque<Result<RunOutput, RunError>>>,
    calls: Mutex<Vec<RunSpec>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, result: Result<RunOutput, RunError>) {
        self.queue.lock().unwrap().push_back(result);
    }

    pub fn calls(&self) -> Vec<RunSpec> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Runner for FakeRunner {
    async fn run(&self, spec: RunSpec) -> Result<RunOutput, RunError> {
        let program = spec.program.clone();
        self.calls.lock().unwrap().push(spec);
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(RunError { kind: RunErrorKind::Spawn, message: format!("FakeRunner: nothing queued for {program}") }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_runner_returns_queued_outputs_in_order_and_records_calls() {
        // Arrange
        let runner = FakeRunner::new();
        runner.push(Ok(RunOutput::ok("one")));
        runner.push(Ok(RunOutput::ok("two")));
        // Act
        let a = runner.run(RunSpec::new("tool", &["-a"], Duration::from_secs(1))).await.unwrap();
        let b = runner.run(RunSpec::new("tool", &["-b"], Duration::from_secs(1))).await.unwrap();
        let empty = runner.run(RunSpec::new("tool", &[], Duration::from_secs(1))).await;
        // Assert
        assert_eq!((a.stdout_text(), b.stdout_text()), ("one".to_string(), "two".to_string()));
        assert!(empty.is_err());
        assert_eq!(runner.calls()[1].args, vec!["-b".to_string()]);
    }

    #[tokio::test]
    async fn process_runner_reports_a_missing_program_as_a_spawn_error() {
        // Arrange
        let spec = RunSpec::new("streamloop-no-such-program", &[], Duration::from_secs(5));
        // Act
        let err = ProcessRunner.run(spec).await.unwrap_err();
        // Assert
        assert_eq!(err.kind, RunErrorKind::Spawn);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn process_runner_captures_output_and_feeds_stdin() {
        // Arrange
        let mut spec = RunSpec::new("findstr", &["x"], Duration::from_secs(10));
        spec.stdin = Some(b"xyz\r\nabc\r\n".to_vec());
        // Act
        let out = ProcessRunner.run(spec).await.unwrap();
        // Assert
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout_text().trim(), "xyz");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn process_runner_kills_a_child_that_outlives_its_timeout() {
        // Arrange
        let spec = RunSpec::new("ping", &["-n", "30", "127.0.0.1"], Duration::from_millis(300));
        // Act
        let err = ProcessRunner.run(spec).await.unwrap_err();
        // Assert
        assert_eq!(err.kind, RunErrorKind::Timeout);
    }
}
