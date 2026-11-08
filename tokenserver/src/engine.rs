use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustypipe_botguard::{Botguard, Error as BgError};
use serde_json::Value;

use crate::homepage::Challenge;

pub trait Engine: Send {
    /// Runs BotGuard on `challenge` with `interpreter_js`, with `globals` set on globalThis first,
    /// and gets an integrity token. Ok(seconds the minter is good for).
    fn start(&mut self, challenge: &Challenge, interpreter_js: &str, globals: &Value) -> Result<u64, String>;
    fn mint(&mut self, content_binding: &str) -> Result<String, String>;
}

/// The crate's GenerateIT request has no timeout of its own. This one cannot stop JavaScript
/// that never yields.
const START_TIMEOUT: Duration = Duration::from_secs(60);
const SNAPSHOT_FILE: &str = "botguard-snapshot.bin";
const STOPPED: &str = "the BotGuard thread stopped";

/// Owns one thread with a current-thread tokio runtime; the crate's JsRuntime never leaves it.
/// The thread starts with the first `start` and again on a later `start` if it has died.
pub struct BotguardEngine {
    user_agent: String,
    data: Option<PathBuf>,
    worker: Option<Worker>,
}

struct Worker {
    jobs: mpsc::Sender<Job>,
    thread: thread::JoinHandle<()>,
}

enum Job {
    Start {
        interpreter_js: String,
        program: String,
        global_name: String,
        globals: Value,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    Mint {
        content_binding: String,
        reply: mpsc::Sender<Result<String, String>>,
    },
}

impl BotguardEngine {
    /// `data`: the folder for a BotGuard snapshot, `None` for no snapshot.
    pub fn new(user_agent: &str, data: Option<&Path>) -> Self {
        Self {
            user_agent: user_agent.to_owned(),
            data: data.map(Path::to_owned),
            worker: None,
        }
    }

    fn live_worker(&mut self) -> Result<&Worker, String> {
        if self.worker.as_ref().is_some_and(|w| w.thread.is_finished()) {
            self.stop_worker();
        }
        if self.worker.is_none() {
            let (jobs, queue) = mpsc::channel();
            let (user_agent, data) = (self.user_agent.clone(), self.data.clone());
            let thread = thread::Builder::new()
                .name("botguard".into())
                .spawn(move || serve(queue, &user_agent, data.as_deref()))
                .map_err(|e| format!("could not start the BotGuard thread: {e}"))?;
            self.worker = Some(Worker { jobs, thread });
        }
        self.worker.as_ref().ok_or_else(|| STOPPED.to_owned())
    }

    fn stop_worker(&mut self) {
        if let Some(Worker { jobs, thread }) = self.worker.take() {
            drop(jobs);
            let _ = thread.join();
        }
    }
}

impl Drop for BotguardEngine {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

fn ask<T>(worker: &Worker, job: impl FnOnce(mpsc::Sender<T>) -> Job) -> Result<T, String> {
    let (reply, answer) = mpsc::channel();
    worker.jobs.send(job(reply)).map_err(|_| STOPPED.to_owned())?;
    answer.recv().map_err(|_| STOPPED.to_owned())
}

impl Engine for BotguardEngine {
    fn start(&mut self, challenge: &Challenge, interpreter_js: &str, globals: &Value) -> Result<u64, String> {
        let worker = self.live_worker()?;
        ask(worker, |reply| Job::Start {
            interpreter_js: interpreter_js.to_owned(),
            program: challenge.program.clone(),
            global_name: challenge.global_name.clone(),
            globals: globals.clone(),
            reply,
        })?
    }

    fn mint(&mut self, content_binding: &str) -> Result<String, String> {
        let worker = self.worker.as_ref().ok_or("no BotGuard minter: start has not succeeded")?;
        ask(worker, |reply| Job::Mint {
            content_binding: content_binding.to_owned(),
            reply,
        })?
    }
}

/// The BotGuard thread: runs jobs until the engine drops its sender.
fn serve(queue: mpsc::Receiver<Job>, user_agent: &str, data: Option<&Path>) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
    let mut minter: Option<Botguard> = None;
    for job in queue {
        let rt = match &rt {
            Ok(rt) => rt,
            Err(e) => {
                job.fail(format!("could not start the BotGuard runtime: {e}"));
                continue;
            }
        };
        match job {
            Job::Start { interpreter_js, program, global_name, globals, reply } => {
                // The old isolate goes first; a failed start leaves no minter behind.
                minter = None;
                let started = rt.block_on(async {
                    let run = run_botguard(user_agent, data, &interpreter_js, &program, &global_name, &globals);
                    tokio::time::timeout(START_TIMEOUT, run)
                        .await
                        .unwrap_or_else(|_| Err(format!("BotGuard did not finish in {} s", START_TIMEOUT.as_secs())))
                });
                let answer = started.and_then(|bg| match seconds_left(&bg) {
                    0 => Err("the BotGuard minter has already expired".to_owned()),
                    left => {
                        minter = Some(bg);
                        Ok(left)
                    }
                });
                let _ = reply.send(answer);
            }
            Job::Mint { content_binding, reply } => {
                let answer = match minter.as_mut() {
                    Some(bg) => rt.block_on(bg.mint_token(&content_binding)).map_err(describe),
                    None => Err("no BotGuard minter: start has not succeeded".to_owned()),
                };
                let _ = reply.send(answer);
            }
        }
    }
}

impl Job {
    fn fail(self, msg: String) {
        match self {
            Job::Start { reply, .. } => drop(reply.send(Err(msg))),
            Job::Mint { reply, .. } => drop(reply.send(Err(msg))),
        }
    }
}

async fn run_botguard(
    user_agent: &str,
    data: Option<&Path>,
    interpreter_js: &str,
    program: &str,
    global_name: &str,
    globals: &Value,
) -> Result<Botguard, String> {
    let builder = || {
        Botguard::builder()
            .user_agent(user_agent)
            .challenge(interpreter_js, program, global_name)
            .globals(globals)
    };
    let Some(dir) = data else {
        return builder().init().await.map_err(describe);
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = dir.join(SNAPSHOT_FILE);
    let bg = builder().snapshot_path(&path).init().await.map_err(describe)?;
    if bg.is_from_snapshot() {
        return Ok(bg);
    }
    // Writing the snapshot consumes the runtime, so the minter kept is the one restored from it.
    if !bg.write_snapshot().await {
        return Err(format!("could not write the BotGuard snapshot to {}", path.display()));
    }
    builder().snapshot_path(&path).init().await.map_err(describe)
}

fn seconds_left(bg: &Botguard) -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    u64::try_from(bg.valid_until().unix_timestamp()).map_or(0, |until| until.saturating_sub(now))
}

/// The crate's error text, except that an invalid PO token is not repeated.
fn describe(e: BgError) -> String {
    match e {
        BgError::InvalidPoToken(_) => "BotGuard minted an invalid PO token".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Stands in for BotGuard. Its VM throws from `a`, BotGuardClient's first call, so nothing
    // reaches GenerateIT; the message reports what the program saw.
    const FAKE_INTERPRETER: &str = r#"
        globalThis.fakeBotGuard = {
            a(program) {
                const w = Object.getOwnPropertyDescriptor(globalThis, "window");
                throw new Error("streamloop-check program=" + program
                    + " event=" + globalThis.yt.config_.EVENT_ID
                    + " window-yt=" + (window.yt === globalThis.yt)
                    + " window-data=" + ("value" in w));
            },
        };
    "#;

    fn fake_challenge() -> Challenge {
        Challenge {
            program: "PROG".into(),
            global_name: "fakeBotGuard".into(),
            interpreter_hash: String::new(),
            interpreter_url: "//www.google.com/js/th/fake.js".into(),
        }
    }

    #[test]
    fn mint_before_a_successful_start_is_an_error() {
        let mut engine = BotguardEngine::new("test-agent", None);
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn start_hands_botguard_the_callers_challenge_with_the_globals_already_set() {
        let mut engine = BotguardEngine::new("test-agent", None);
        let globals = json!({"yt": {"config_": {"EVENT_ID": "e1"}}});

        let err = engine.start(&fake_challenge(), FAKE_INTERPRETER, &globals).unwrap_err();

        assert!(
            err.contains("streamloop-check program=PROG event=e1 window-yt=true window-data=true"),
            "{err}"
        );
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn start_refuses_globals_that_are_not_an_object() {
        let mut engine = BotguardEngine::new("test-agent", None);
        let err = engine.start(&fake_challenge(), FAKE_INTERPRETER, &json!([1])).unwrap_err();
        assert!(err.contains("globals must be a JSON object"), "{err}");
    }
}
