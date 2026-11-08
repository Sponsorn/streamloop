use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustypipe_botguard::{Botguard, Error as BgError, IsolateHandle};
use serde_json::Value;

use crate::homepage::Challenge;

pub trait Engine: Send {
    /// Runs BotGuard on `challenge` with `interpreter_js`, with `globals` set on globalThis first,
    /// and gets an integrity token. Ok(seconds the minter is good for).
    fn start(&mut self, challenge: &Challenge, interpreter_js: &str, globals: &Value) -> Result<u64, String>;
    fn mint(&mut self, content_binding: &str) -> Result<String, String>;
}

#[derive(Clone, Copy)]
struct Limits {
    /// Enforced on the BotGuard thread, where only an await can be cut short.
    start: Duration,
    mint: Duration,
    /// Added to a job's own limit before the caller gives up on the thread.
    margin: Duration,
}

const LIMITS: Limits = Limits {
    start: Duration::from_secs(60),
    mint: Duration::from_secs(20),
    margin: Duration::from_secs(10),
};
const SNAPSHOT_FILE: &str = "botguard-snapshot.bin";
const STOPPED: &str = "the BotGuard thread stopped";
const NO_MINTER: &str = "no BotGuard minter: start has not succeeded";

type IsolateSlot = Arc<Mutex<Option<IsolateHandle>>>;

/// Owns one thread with a current-thread tokio runtime; the crate's JsRuntime never leaves it.
/// A `start` spawns the thread when there is none, or when it has died or been abandoned.
pub struct BotguardEngine {
    user_agent: String,
    data: Option<PathBuf>,
    limits: Limits,
    worker: Option<Worker>,
    #[cfg(test)]
    abandoned: Vec<thread::JoinHandle<()>>,
}

struct Worker {
    jobs: mpsc::Sender<Job>,
    thread: thread::JoinHandle<()>,
    /// The isolate the thread created last, so a stuck call can be terminated from outside.
    isolate: IsolateSlot,
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
    #[cfg(test)]
    Install {
        minter: TestMinter,
        reply: mpsc::Sender<Result<(), String>>,
    },
}

enum Minter {
    Botguard(Botguard),
    #[cfg(test)]
    Test(TestMinter),
}

impl BotguardEngine {
    /// `data`: the folder for a BotGuard snapshot, `None` for no snapshot.
    pub fn new(user_agent: &str, data: Option<&Path>) -> Self {
        Self::with_limits(user_agent, data, LIMITS)
    }

    fn with_limits(user_agent: &str, data: Option<&Path>, limits: Limits) -> Self {
        Self {
            user_agent: user_agent.to_owned(),
            data: data.map(Path::to_owned),
            limits,
            worker: None,
            #[cfg(test)]
            abandoned: Vec::new(),
        }
    }

    fn ensure_worker(&mut self) -> Result<(), String> {
        if self.worker.as_ref().is_some_and(|w| w.thread.is_finished()) {
            self.stop_worker();
        }
        if self.worker.is_none() {
            let (jobs, queue) = mpsc::channel();
            let isolate = IsolateSlot::default();
            let (user_agent, data, limits) = (self.user_agent.clone(), self.data.clone(), self.limits);
            let slot = isolate.clone();
            let thread = thread::Builder::new()
                .name("botguard".into())
                .spawn(move || serve(queue, &user_agent, data.as_deref(), limits, &slot))
                .map_err(|e| format!("could not start the BotGuard thread: {e}"))?;
            self.worker = Some(Worker { jobs, thread, isolate });
        }
        Ok(())
    }

    /// Joins the thread. Only ever called for a thread that is idle or finished: a call that
    /// does not come back abandons its thread instead.
    fn stop_worker(&mut self) {
        if let Some(Worker { jobs, thread, .. }) = self.worker.take() {
            drop(jobs);
            let _ = thread.join();
        }
    }

    /// Stops the isolate's JavaScript and lets the thread go without joining it. It ends
    /// once its job returns, since its queue is gone; a new thread serves the next `start`.
    fn abandon_worker(&mut self) {
        if let Some(Worker { jobs, thread, isolate }) = self.worker.take() {
            if let Some(handle) = isolate.lock().unwrap_or_else(PoisonError::into_inner).take() {
                handle.terminate_execution();
            }
            drop(jobs);
            #[cfg(test)]
            self.abandoned.push(thread);
            #[cfg(not(test))]
            drop(thread);
        }
    }

    /// Sends a job and waits up to its limit plus the margin. A thread whose queue is closed is
    /// let go; with `respawn`, the job, which never ran, goes to a new thread.
    fn ask<T>(&mut self, limit: Duration, respawn: bool, job: impl FnOnce(mpsc::Sender<T>) -> Job) -> Result<T, String> {
        let (reply, answer) = mpsc::channel();
        let mut job = job(reply);
        loop {
            let worker = self.worker.as_ref().ok_or(NO_MINTER)?;
            match worker.jobs.send(job) {
                Ok(()) => break,
                Err(mpsc::SendError(undelivered)) => {
                    self.worker = None;
                    if !respawn {
                        return Err(STOPPED.to_owned());
                    }
                    self.ensure_worker()?;
                    job = undelivered;
                }
            }
        }
        let wait = limit + self.limits.margin;
        match answer.recv_timeout(wait) {
            Ok(answer) => Ok(answer),
            Err(RecvTimeoutError::Disconnected) => {
                self.worker = None;
                Err(STOPPED.to_owned())
            }
            Err(RecvTimeoutError::Timeout) => {
                self.abandon_worker();
                Err(format!("BotGuard did not answer in {} s; its thread was abandoned", wait.as_secs_f64()))
            }
        }
    }

    #[cfg(test)]
    fn install_test_minter(&mut self, minter: TestMinter) -> Result<(), String> {
        self.ensure_worker()?;
        let limit = self.limits.mint;
        self.ask(limit, false, |reply| Job::Install { minter, reply })?
    }
}

impl Drop for BotguardEngine {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

impl Engine for BotguardEngine {
    fn start(&mut self, challenge: &Challenge, interpreter_js: &str, globals: &Value) -> Result<u64, String> {
        self.ensure_worker()?;
        let limit = self.limits.start;
        self.ask(limit, true, |reply| Job::Start {
            interpreter_js: interpreter_js.to_owned(),
            program: challenge.program.clone(),
            global_name: challenge.global_name.clone(),
            globals: globals.clone(),
            reply,
        })?
    }

    fn mint(&mut self, content_binding: &str) -> Result<String, String> {
        let limit = self.limits.mint;
        self.ask(limit, false, |reply| Job::Mint {
            content_binding: content_binding.to_owned(),
            reply,
        })?
    }
}

/// The BotGuard thread. After a panic the isolate counts as broken: the queue closes before
/// the answer goes back, so the engine's next job finds the thread gone.
fn serve(queue: mpsc::Receiver<Job>, user_agent: &str, data: Option<&Path>, limits: Limits, isolate: &IsolateSlot) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            for job in queue {
                job.fail(format!("could not start the BotGuard runtime: {e}"));
            }
            return;
        }
    };
    let mut minter: Option<Minter> = None;
    loop {
        let Ok(job) = queue.recv() else { return };
        match job {
            Job::Start { interpreter_js, program, global_name, globals, reply } => {
                // The old isolate goes first; a failed start leaves no minter behind.
                minter = None;
                let (answer, panicked) = guarded(|| {
                    let run = run_botguard(user_agent, data, isolate, &interpreter_js, &program, &global_name, &globals);
                    let bg = rt.block_on(async {
                        tokio::time::timeout(limits.start, run)
                            .await
                            .unwrap_or_else(|_| Err(format!("BotGuard did not finish in {} s", limits.start.as_secs_f64())))
                    })?;
                    match seconds_left(&bg) {
                        0 => Err("the BotGuard minter has already expired".to_owned()),
                        left => {
                            minter = Some(Minter::Botguard(bg));
                            Ok(left)
                        }
                    }
                });
                if panicked {
                    drop(queue);
                    let _ = reply.send(answer);
                    return;
                }
                let _ = reply.send(answer);
            }
            Job::Mint { content_binding, reply } => {
                let (answer, panicked) = guarded(|| mint_with(&rt, &mut minter, &content_binding, limits.mint));
                if panicked {
                    drop(queue);
                    let _ = reply.send(answer);
                    return;
                }
                let _ = reply.send(answer);
            }
            #[cfg(test)]
            Job::Install { minter: test_minter, reply } => {
                minter = Some(Minter::Test(test_minter));
                let _ = reply.send(Ok(()));
            }
        }
    }
}

/// Runs `f`, turning a panic into an Err with its message. The flag says it panicked.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> (Result<T, String>, bool) {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(answer) => (answer, false),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".to_owned());
            (Err(format!("BotGuard panicked: {msg}")), true)
        }
    }
}

/// A mint that runs out of time drops the minter, since its isolate may still be busy.
fn mint_with(rt: &tokio::runtime::Runtime, minter: &mut Option<Minter>, content_binding: &str, limit: Duration) -> Result<String, String> {
    let answer = match minter.as_mut().ok_or(NO_MINTER)? {
        Minter::Botguard(bg) => rt
            .block_on(async { tokio::time::timeout(limit, bg.mint_token(content_binding)).await })
            .map(|minted| minted.map_err(describe)),
        #[cfg(test)]
        Minter::Test(test_minter) => test_minter.mint(rt, limit),
    };
    answer.unwrap_or_else(|_| {
        *minter = None;
        Err(format!("BotGuard did not mint in {} s", limit.as_secs_f64()))
    })
}

impl Job {
    fn fail(self, msg: String) {
        match self {
            Job::Start { reply, .. } => drop(reply.send(Err(msg))),
            Job::Mint { reply, .. } => drop(reply.send(Err(msg))),
            #[cfg(test)]
            Job::Install { reply, .. } => drop(reply.send(Err(msg))),
        }
    }
}

async fn run_botguard(
    user_agent: &str,
    data: Option<&Path>,
    isolate: &IsolateSlot,
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
            .isolate_slot(isolate)
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

/// Minters that misbehave on purpose, for the engine's own tests.
#[cfg(test)]
#[derive(Clone, Copy)]
enum TestMinter {
    NeverSettles,
    /// A mint that holds the thread without yielding, as JavaScript in a loop would.
    Blocks(Duration),
    Panics,
}

#[cfg(test)]
impl TestMinter {
    fn mint(self, rt: &tokio::runtime::Runtime, limit: Duration) -> Result<Result<String, String>, tokio::time::error::Elapsed> {
        match self {
            TestMinter::NeverSettles => rt.block_on(async { tokio::time::timeout(limit, std::future::pending()).await }),
            TestMinter::Blocks(d) => {
                thread::sleep(d);
                Ok(Ok("late".to_owned()))
            }
            TestMinter::Panics => panic!("test minter panicked"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    // Stands in for BotGuard. Its VM throws from `a`, BotGuardClient's first call, so nothing
    // reaches GenerateIT; the message reports what the program saw.
    const FAKE_INTERPRETER: &str = r#"
        globalThis.fakeBotGuard = {
            a(program) {
                const w = Object.getOwnPropertyDescriptor(globalThis, "window");
                throw new Error("streamloop-check program=" + program
                    + " event=" + globalThis.yt.config_.EVENT_ID
                    + " window-yt=" + (window.yt === globalThis.yt)
                    + " window-data=" + ("value" in w)
                    + " ua=" + window.navigator.userAgent);
            },
        };
    "#;

    const SPINNING_INTERPRETER: &str = "globalThis.fakeBotGuard = { a() { for (;;) {} } };";

    // The interval keeps the event loop busy, so deno_core never treats the promise as stalled.
    const PENDING_INTERPRETER: &str =
        "globalThis.fakeBotGuard = { a() { setInterval(() => {}, 50); return [new Promise(() => {})]; } };";

    const SHORT: Limits = Limits {
        start: Duration::from_millis(1500),
        mint: Duration::from_millis(500),
        margin: Duration::from_millis(500),
    };

    fn fake_challenge() -> Challenge {
        Challenge {
            program: "PROG".into(),
            global_name: "fakeBotGuard".into(),
            interpreter_hash: String::new(),
            interpreter_url: "//www.google.com/js/th/fake.js".into(),
        }
    }

    fn globals() -> Value {
        json!({"yt": {"config_": {"EVENT_ID": "e1"}}})
    }

    /// A start that reaches the fake BotGuard, which only a working thread can do.
    fn assert_start_reaches_botguard(engine: &mut BotguardEngine) {
        let err = engine.start(&fake_challenge(), FAKE_INTERPRETER, &globals()).unwrap_err();
        assert!(err.contains("streamloop-check program=PROG"), "{err}");
    }

    fn wait_until_finished(handle: &thread::JoinHandle<()>, within: Duration) -> bool {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if handle.is_finished() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        handle.is_finished()
    }

    #[test]
    fn mint_before_a_successful_start_is_an_error() {
        let mut engine = BotguardEngine::new("test-agent", None);
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn start_hands_botguard_the_callers_challenge_with_the_globals_already_set() {
        let mut engine = BotguardEngine::new("test-agent", None);

        let err = engine.start(&fake_challenge(), FAKE_INTERPRETER, &globals()).unwrap_err();

        assert!(
            err.contains("streamloop-check program=PROG event=e1 window-yt=true window-data=true ua=test-agent"),
            "{err}"
        );
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
    }

    #[test]
    fn globals_over_128_kb_reach_botguard_with_and_without_a_snapshot() {
        let dir = std::env::temp_dir().join(format!("streamloop-pot-test-{}", std::process::id()));
        for data in [None, Some(dir.as_path())] {
            let mut engine = BotguardEngine::new("test-agent", data);
            for kb in [1, 60, 100, 200] {
                let globals = json!({"yt": {"config_": {"EVENT_ID": "e1", "PAD": "x".repeat(kb * 1024)}}});
                let err = engine.start(&fake_challenge(), FAKE_INTERPRETER, &globals).unwrap_err();
                assert!(err.contains("event=e1 window-yt=true"), "{kb} KB, snapshot {}: {err}", data.is_some());
            }
        }
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn start_refuses_globals_that_are_not_an_object() {
        let mut engine = BotguardEngine::new("test-agent", None);
        let err = engine.start(&fake_challenge(), FAKE_INTERPRETER, &json!([1])).unwrap_err();
        assert!(err.contains("globals must be a JSON object"), "{err}");
    }

    #[test]
    fn a_start_whose_javascript_never_returns_is_terminated_and_the_next_start_gets_a_new_thread() {
        let mut engine = BotguardEngine::with_limits("test-agent", None, SHORT);

        let t = Instant::now();
        let err = engine.start(&fake_challenge(), SPINNING_INTERPRETER, &globals()).unwrap_err();

        assert!(t.elapsed() < Duration::from_secs(5), "took {:?}", t.elapsed());
        assert!(err.contains("abandoned"), "{err}");
        assert!(wait_until_finished(&engine.abandoned[0], Duration::from_secs(5)), "the spinning thread still runs");
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
        assert_start_reaches_botguard(&mut engine);
    }

    #[test]
    fn a_start_whose_promise_never_settles_times_out_on_the_thread() {
        let mut engine = BotguardEngine::with_limits("test-agent", None, SHORT);

        let err = engine.start(&fake_challenge(), PENDING_INTERPRETER, &globals()).unwrap_err();

        assert!(err.contains("did not finish"), "{err}");
        assert!(engine.abandoned.is_empty());
        assert_start_reaches_botguard(&mut engine);
    }

    #[test]
    fn a_mint_that_never_settles_fails_within_its_limit_and_drops_the_minter() {
        let mut engine = BotguardEngine::with_limits("test-agent", None, SHORT);
        engine.install_test_minter(TestMinter::NeverSettles).unwrap();

        let t = Instant::now();
        let err = engine.mint("dQw4w9WgXcQ").unwrap_err();

        assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
        assert!(err.contains("did not mint"), "{err}");
        assert!(engine.mint("dQw4w9WgXcQ").unwrap_err().contains("no BotGuard minter"));
        assert_start_reaches_botguard(&mut engine);
    }

    #[test]
    fn a_mint_that_holds_the_thread_is_abandoned_and_the_next_start_gets_a_new_thread() {
        let mut engine = BotguardEngine::with_limits("test-agent", None, SHORT);
        engine.install_test_minter(TestMinter::Blocks(Duration::from_secs(4))).unwrap();

        let t = Instant::now();
        let err = engine.mint("dQw4w9WgXcQ").unwrap_err();

        assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
        assert!(err.contains("abandoned"), "{err}");
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
        assert_start_reaches_botguard(&mut engine);
    }

    #[test]
    fn a_panic_on_the_thread_comes_back_as_an_error_and_the_next_start_gets_a_new_thread() {
        let mut engine = BotguardEngine::with_limits("test-agent", None, SHORT);
        engine.install_test_minter(TestMinter::Panics).unwrap();

        let err = engine.mint("dQw4w9WgXcQ").unwrap_err();

        assert!(err.contains("BotGuard panicked: test minter panicked"), "{err}");
        assert!(engine.mint("dQw4w9WgXcQ").is_err());
        assert_start_reaches_botguard(&mut engine);
    }
}
