// Port of bgutil-ytdlp-pot-provider 2.0.0's session_manager.ts minter.
// Copyright (c) Brainicism and contributors, GPL-3.0.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::engine::Engine;
use crate::google::Web;
use crate::homepage::{challenge_from_json, parse_challenge, parse_ytcfg, Challenge};

/// What `expires_at` reports for a fresh token, as 2.0.0 does.
pub const TOKEN_TTL: Duration = Duration::from_secs(6 * 3600);

/// 2.0.0 tries every fetch 3 times, 5 s apart, before it gives up on it.
const FETCH_TRIES: u32 = 3;
const FETCH_RETRY_DELAY: Duration = Duration::from_secs(5);

pub struct PotRequest {
    pub content_binding: String,
    pub challenge: Option<Value>,
    pub bypass_cache: bool,
    pub innertube_context: Option<Value>,
}

pub struct PotAnswer {
    pub content_binding: String,
    pub po_token: String,
    pub expires_at: SystemTime,
}

type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

#[derive(Default)]
struct State {
    /// When the live minter runs out; None while there is none.
    minter_expiry: Option<SystemTime>,
    /// Bumped by the invalidations, so a start that began before one does not outlive it.
    generation: u64,
    /// Counts finished start attempts; `start_error` is the last one's failure, if it failed.
    start_epoch: u64,
    start_error: Option<String>,
}

/// One minter. Every request mints a fresh token from it: 2.0.0 as shipped caches none.
pub struct Sessions {
    /// Held across a whole start and mint, so requests that find no live minter wait on one
    /// start. `minter_keys` and the invalidations read `state` and never wait here.
    engine: Mutex<Box<dyn Engine>>,
    web: Box<dyn Web>,
    clock: Clock,
    retry_delay: Duration,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Sessions {
    pub fn new(engine: Box<dyn Engine>, web: Box<dyn Web>, clock: Clock) -> Self {
        Self {
            engine: Mutex::new(engine),
            web,
            clock,
            retry_delay: FETCH_RETRY_DELAY,
            state: Mutex::new(State::default()),
        }
    }

    /// Tests use this so a retried fetch does not sleep for seconds.
    #[cfg(test)]
    fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    pub fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        let result = self.get_pot_inner(r);
        let id = binding_of(r).unwrap_or("(none)");
        match &result {
            Ok(_) => println!("get_pot {id}: ok"),
            Err(e) => println!("get_pot {id}: error {e}"),
        }
        result
    }

    /// Drops the minter.
    pub fn invalidate_caches(&self) {
        self.drop_minter();
    }

    /// The minter counts as expired.
    pub fn invalidate_it(&self) {
        self.drop_minter();
    }

    pub fn minter_keys(&self) -> Vec<String> {
        if self.minter_live() {
            vec!["default".to_owned()]
        } else {
            Vec::new()
        }
    }

    fn drop_minter(&self) {
        let mut st = lock(&self.state);
        st.generation += 1;
        st.minter_expiry = None;
    }

    fn get_pot_inner(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        let binding = binding_of(r).ok_or("no content_binding and no visitorData in the innertube context")?;
        let seen_epoch = lock(&self.state).start_epoch;
        let mut engine = lock(&self.engine);
        let (generation, epoch_moved, start_error) = {
            let st = lock(&self.state);
            (st.generation, st.start_epoch != seen_epoch, st.start_error.clone())
        };
        // A start finished while this request waited: its failure is this request's failure,
        // and its minter is this request's minter, bypass or not.
        if let Some(e) = start_error.filter(|_| epoch_moved) {
            return Err(e);
        }
        if !self.minter_live() || (r.bypass_cache && !epoch_moved) {
            let started = self.start(engine.as_mut(), r);
            let mut st = lock(&self.state);
            st.start_epoch += 1;
            match started {
                Ok(valid_for_s) => {
                    st.start_error = None;
                    if st.generation == generation {
                        st.minter_expiry = Some((self.clock)() + Duration::from_secs(valid_for_s));
                    }
                }
                Err(e) => {
                    st.minter_expiry = None;
                    st.start_error = Some(e.clone());
                    return Err(e);
                }
            }
        }
        let token = match engine.mint(binding) {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => return Err(self.mint_failed(binding, "empty token")),
            Err(e) => return Err(self.mint_failed(binding, &e)),
        };
        Ok(PotAnswer {
            content_binding: binding.to_owned(),
            po_token: token,
            expires_at: (self.clock)() + TOKEN_TTL,
        })
    }

    /// A failed mint drops the minter, so the next request starts over.
    fn mint_failed(&self, binding: &str, why: &str) -> String {
        lock(&self.state).minter_expiry = None;
        format!("mint for {binding}: {why}")
    }

    fn minter_live(&self) -> bool {
        let now = (self.clock)();
        matches!(lock(&self.state).minter_expiry, Some(exp) if now < exp)
    }

    /// Runs `f` up to FETCH_TRIES times, `retry_delay` apart.
    fn fetch_retried<T>(&self, f: impl Fn() -> Result<T, String>) -> Result<T, String> {
        let mut tries = 1;
        loop {
            match f() {
                Err(_) if tries < FETCH_TRIES => {
                    tries += 1;
                    thread::sleep(self.retry_delay);
                }
                other => return other,
            }
        }
    }

    /// Fetches the challenge and its interpreter once and starts the engine on them.
    fn start(&self, engine: &mut dyn Engine, r: &PotRequest) -> Result<u64, String> {
        let (challenge, globals, source) = match self.homepage_pair() {
            Ok((c, ytcfg)) => (c, json!({"yt": {"config_": ytcfg}}), "homepage"),
            Err(why) => match r.challenge.as_ref() {
                Some(v) => {
                    println!("minter: homepage failed ({why}), using the plugin's challenge");
                    let c = challenge_from_json(v).ok_or_else(|| {
                        "the request's challenge lacks program, globalName or interpreterUrl".to_owned()
                    })?;
                    (c, json!({}), "plugin")
                }
                None => {
                    println!("minter: homepage failed ({why}), using att/get");
                    let c = self
                        .fetch_retried(|| self.web.att_get(r.innertube_context.as_ref()))
                        .map_err(|e| format!("att/get: {e}"))?;
                    (c, json!({}), "att/get")
                }
            },
        };
        let interpreter_js = self
            .fetch_retried(|| self.web.interpreter(&challenge.interpreter_url))
            .map_err(|e| format!("interpreter: {e}"))?;
        let valid_for_s = engine
            .start(&challenge, &interpreter_js, &globals)
            .map_err(|e| format!("BotGuard start: {e}"))?;
        println!("minter: {source} challenge, valid {valid_for_s} s");
        Ok(valid_for_s)
    }

    fn homepage_pair(&self) -> Result<(Challenge, Value), String> {
        let html = self
            .fetch_retried(|| self.web.homepage())
            .map_err(|e| format!("homepage fetch: {e}"))?;
        let ytcfg = parse_ytcfg(&html).ok_or("no ytcfg on the homepage")?;
        let challenge = parse_challenge(&html).ok_or("no challenge on the homepage")?;
        Ok((challenge, ytcfg))
    }
}

/// The request's binding, else the innertube context's visitorData, as 2.0.0 does.
fn binding_of(r: &PotRequest) -> Option<&str> {
    if !r.content_binding.is_empty() {
        return Some(&r.content_binding);
    }
    r.innertube_context
        .as_ref()?
        .get("client")?
        .get("visitorData")?
        .as_str()
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::UNIX_EPOCH;

    #[derive(Default)]
    struct EngineLog {
        starts: usize,
        mints: usize,
        globals: Vec<Value>,
        programs: Vec<String>,
        fail_next_start: bool,
        fail_next_mint: bool,
        start_sleep_ms: u64,
    }

    struct FakeEngine(Arc<Mutex<EngineLog>>);

    impl Engine for FakeEngine {
        fn start(&mut self, c: &Challenge, _js: &str, globals: &Value) -> Result<u64, String> {
            let (sleep, fail) = {
                let mut l = self.0.lock().unwrap();
                l.starts += 1;
                l.globals.push(globals.clone());
                l.programs.push(c.program.clone());
                (l.start_sleep_ms, std::mem::take(&mut l.fail_next_start))
            };
            thread::sleep(Duration::from_millis(sleep));
            if fail {
                return Err("boom".to_owned());
            }
            Ok(43200)
        }
        fn mint(&mut self, binding: &str) -> Result<String, String> {
            let mut l = self.0.lock().unwrap();
            l.mints += 1;
            if std::mem::take(&mut l.fail_next_mint) {
                return Err("mint boom".to_owned());
            }
            Ok(format!("tok{}-{binding}", l.mints))
        }
    }

    struct WebLog {
        /// One answer per homepage call; the last repeats.
        homepage_script: Mutex<Vec<Result<String, String>>>,
        homepages: AtomicUsize,
        att_gets: AtomicUsize,
        interpreter_failures: AtomicUsize,
        interpreters: AtomicUsize,
    }

    struct FakeWeb(Arc<WebLog>);

    impl Web for FakeWeb {
        fn homepage(&self) -> Result<String, String> {
            self.0.homepages.fetch_add(1, Ordering::SeqCst);
            let mut s = self.0.homepage_script.lock().unwrap();
            if s.len() > 1 {
                s.remove(0)
            } else {
                s[0].clone()
            }
        }
        fn interpreter(&self, _url: &str) -> Result<String, String> {
            self.0.interpreters.fetch_add(1, Ordering::SeqCst);
            let left = self.0.interpreter_failures.load(Ordering::SeqCst);
            if left > 0 {
                self.0.interpreter_failures.store(left - 1, Ordering::SeqCst);
                return Err("interpreter blip".to_owned());
            }
            Ok("interpreter".to_owned())
        }
        fn att_get(&self, _ctx: Option<&Value>) -> Result<Challenge, String> {
            self.0.att_gets.fetch_add(1, Ordering::SeqCst);
            Ok(Challenge {
                program: "att-program".to_owned(),
                global_name: "g".to_owned(),
                interpreter_hash: "h".to_owned(),
                interpreter_url: "//www.google.com/js/i.js".to_owned(),
            })
        }
    }

    const PAGE: &str = r#"<script>ytcfg.set({"EVENT_ID":"e1"});</script>
<script>window.ytAtN({R:{bgChallenge:{program:"page-program",globalName:"g",interpreterHash:"h",interpreterUrl:{privateDoNotAccessOrElseTrustedResourceUrlWrappedValue:"//www.google.com/js/i.js"}}}})</script>"#;

    const PAGE_WITHOUT_YTCFG: &str = r#"<script>window.ytAtN({R:{bgChallenge:{program:"page-program",globalName:"g",interpreterHash:"h",interpreterUrl:{privateDoNotAccessOrElseTrustedResourceUrlWrappedValue:"//www.google.com/js/i.js"}}}})</script>"#;

    struct Rig {
        sessions: Arc<Sessions>,
        engine: Arc<Mutex<EngineLog>>,
        web: Arc<WebLog>,
        now: Arc<Mutex<SystemTime>>,
    }

    fn rig(page: Result<&str, &str>) -> Rig {
        rig_script(vec![page.map(str::to_owned).map_err(str::to_owned)])
    }

    fn rig_script(script: Vec<Result<String, String>>) -> Rig {
        let engine = Arc::new(Mutex::new(EngineLog::default()));
        let web = Arc::new(WebLog {
            homepage_script: Mutex::new(script),
            homepages: AtomicUsize::new(0),
            att_gets: AtomicUsize::new(0),
            interpreter_failures: AtomicUsize::new(0),
            interpreters: AtomicUsize::new(0),
        });
        let now = Arc::new(Mutex::new(UNIX_EPOCH + Duration::from_secs(1_000_000)));
        let clock_now = now.clone();
        let sessions = Arc::new(
            Sessions::new(
                Box::new(FakeEngine(engine.clone())),
                Box::new(FakeWeb(web.clone())),
                Arc::new(move || *clock_now.lock().unwrap()),
            )
            .with_retry_delay(Duration::ZERO),
        );
        Rig { sessions, engine, web, now }
    }

    fn advance(r: &Rig, s: u64) {
        *r.now.lock().unwrap() += Duration::from_secs(s);
    }

    fn req(binding: &str) -> PotRequest {
        PotRequest { content_binding: binding.to_owned(), challenge: None, bypass_cache: false, innertube_context: None }
    }

    fn plugin_challenge() -> Value {
        json!({"program": "plugin-program", "globalName": "g", "interpreterHash": "h",
               "interpreterUrl": {"privateDoNotAccessOrElseTrustedResourceUrlWrappedValue": "//www.google.com/js/i.js"}})
    }

    #[test]
    fn mints_from_the_homepage_challenge_with_ytcfg_as_globals() {
        let r = rig(Ok(PAGE));

        let a = r.sessions.get_pot(&req("vid1")).unwrap();

        assert_eq!(a.po_token, "tok1-vid1");
        assert_eq!(a.expires_at, *r.now.lock().unwrap() + TOKEN_TTL);
        let l = r.engine.lock().unwrap();
        assert_eq!(l.globals, vec![json!({"yt": {"config_": {"EVENT_ID": "e1"}}})]);
        assert_eq!(l.programs, vec!["page-program"]);
        assert_eq!(r.sessions.minter_keys(), vec!["default"]);
    }

    #[test]
    fn the_homepage_wins_over_a_challenge_carried_in_the_request() {
        let r = rig(Ok(PAGE));
        let mut with_challenge = req("vid1");
        with_challenge.challenge = Some(plugin_challenge());

        r.sessions.get_pot(&with_challenge).unwrap();

        assert_eq!(r.engine.lock().unwrap().programs, vec!["page-program"]);
    }

    #[test]
    fn falls_back_to_the_request_challenge_then_att_get() {
        let r = rig(Err("offline"));
        let mut with_challenge = req("vid1");
        with_challenge.challenge = Some(plugin_challenge());

        r.sessions.get_pot(&with_challenge).unwrap();
        r.sessions.invalidate_caches();
        r.sessions.get_pot(&req("vid2")).unwrap();

        let l = r.engine.lock().unwrap();
        assert_eq!(l.programs, vec!["plugin-program", "att-program"]);
        assert_eq!(l.globals, vec![json!({}), json!({})]);
        assert_eq!(r.web.att_gets.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_homepage_without_ytcfg_falls_back() {
        let r = rig(Ok(PAGE_WITHOUT_YTCFG));
        let mut with_challenge = req("vid1");
        with_challenge.challenge = Some(plugin_challenge());

        r.sessions.get_pot(&with_challenge).unwrap();

        let l = r.engine.lock().unwrap();
        assert_eq!(l.programs, vec!["plugin-program"]);
        assert_eq!(l.globals, vec![json!({})]);
        assert_eq!(r.web.homepages.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_homepage_fetch_is_tried_three_times_before_the_fallback() {
        let blip = || Err("blip".to_owned());
        let r = rig_script(vec![blip(), blip(), Ok(PAGE.to_owned())]);
        r.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(r.web.homepages.load(Ordering::SeqCst), 3);
        assert_eq!(r.engine.lock().unwrap().programs, vec!["page-program"]);

        let down = rig(Err("down"));
        down.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(down.web.homepages.load(Ordering::SeqCst), 3);
        assert_eq!(down.web.att_gets.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_interpreter_fetch_is_tried_three_times() {
        let r = rig(Ok(PAGE));
        r.web.interpreter_failures.store(2, Ordering::SeqCst);

        assert!(r.sessions.get_pot(&req("a")).is_ok());
        assert_eq!(r.web.interpreters.load(Ordering::SeqCst), 3);

        let r = rig(Ok(PAGE));
        r.web.interpreter_failures.store(3, Ordering::SeqCst);
        let e = r.sessions.get_pot(&req("a")).err().unwrap();
        assert!(e.contains("interpreter") && e.contains("blip"), "{e}");
        assert_eq!(r.engine.lock().unwrap().starts, 0);
    }

    #[test]
    fn keeps_the_minter_until_its_lifetime_runs_out() {
        let r = rig(Ok(PAGE));

        r.sessions.get_pot(&req("a")).unwrap();
        advance(&r, 43199);
        r.sessions.get_pot(&req("b")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 1);
        assert_eq!(r.sessions.minter_keys(), vec!["default"]);

        advance(&r, 2);
        assert!(r.sessions.minter_keys().is_empty());
        r.sessions.get_pot(&req("c")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 2);
        assert_eq!(r.web.homepages.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn every_request_mints_a_new_token_from_the_live_minter() {
        let r = rig(Ok(PAGE));

        let first = r.sessions.get_pot(&req("a")).unwrap();
        let again = r.sessions.get_pot(&req("a")).unwrap();

        assert_ne!(first.po_token, again.po_token);
        let l = r.engine.lock().unwrap();
        assert_eq!((l.starts, l.mints), (1, 2));
    }

    #[test]
    fn bypass_cache_builds_a_new_minter() {
        let r = rig(Ok(PAGE));
        r.sessions.get_pot(&req("a")).unwrap();

        r.sessions.get_pot(&PotRequest { bypass_cache: true, ..req("a") }).unwrap();

        let l = r.engine.lock().unwrap();
        assert_eq!((l.starts, l.mints), (2, 2));
        assert_eq!(r.web.homepages.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalidate_it_and_invalidate_caches_both_force_a_new_minter() {
        let r = rig(Ok(PAGE));
        r.sessions.get_pot(&req("a")).unwrap();

        r.sessions.invalidate_it();
        assert!(r.sessions.minter_keys().is_empty());
        r.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 2);

        r.sessions.invalidate_caches();
        assert!(r.sessions.minter_keys().is_empty());
        r.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 3);
    }

    #[test]
    fn two_concurrent_first_requests_start_one_minter() {
        let r = rig(Ok(PAGE));
        r.engine.lock().unwrap().start_sleep_ms = 500;

        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|b| {
                let s = r.sessions.clone();
                thread::spawn(move || s.get_pot(&req(b)))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        assert!(results.iter().all(Result::is_ok));
        assert_eq!(r.engine.lock().unwrap().starts, 1);
        assert_eq!(r.web.homepages.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn minter_keys_and_invalidation_do_not_wait_behind_a_slow_start() {
        let r = rig(Ok(PAGE));
        r.engine.lock().unwrap().start_sleep_ms = 600;
        let s = r.sessions.clone();
        let slow = thread::spawn(move || s.get_pot(&req("new")));
        thread::sleep(Duration::from_millis(150));

        let t = std::time::Instant::now();
        r.sessions.minter_keys();
        r.sessions.invalidate_it();
        r.sessions.invalidate_caches();

        assert!(t.elapsed() < Duration::from_millis(300));
        slow.join().unwrap().unwrap();
    }

    #[test]
    fn an_engine_failure_is_an_error_and_the_next_request_tries_again() {
        let r = rig(Ok(PAGE));
        r.engine.lock().unwrap().fail_next_start = true;

        let first = r.sessions.get_pot(&req("a"));
        assert!(first.as_ref().is_err_and(|e| e.contains("BotGuard start") && e.contains("boom")));
        assert!(r.sessions.minter_keys().is_empty());
        let second = r.sessions.get_pot(&req("a"));

        assert!(second.is_ok());
        assert_eq!(r.engine.lock().unwrap().starts, 2);
    }

    #[test]
    fn a_failed_start_is_shared_with_the_requests_that_waited_on_it() {
        let r = rig(Ok(PAGE));
        {
            let mut l = r.engine.lock().unwrap();
            l.start_sleep_ms = 400;
            l.fail_next_start = true;
        }
        let handles: Vec<_> = ["a", "b", "c", "d"]
            .into_iter()
            .map(|b| {
                let s = r.sessions.clone();
                let h = thread::spawn(move || s.get_pot(&req(b)));
                thread::sleep(Duration::from_millis(40));
                h
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        assert_eq!(r.engine.lock().unwrap().starts, 1);
        for res in &results {
            assert!(res.as_ref().is_err_and(|e| e.contains("BotGuard start: boom")), "{:?}", res.as_ref().err());
        }

        r.engine.lock().unwrap().start_sleep_ms = 0;
        assert!(r.sessions.get_pot(&req("e")).is_ok());
        assert_eq!(r.engine.lock().unwrap().starts, 2);
    }

    #[test]
    fn a_failed_mint_drops_the_minter() {
        let r = rig(Ok(PAGE));
        r.sessions.get_pot(&req("a")).unwrap();
        r.engine.lock().unwrap().fail_next_mint = true;

        let e = r.sessions.get_pot(&req("a")).err().unwrap();

        assert!(e.contains("mint for a") && e.contains("mint boom"), "{e}");
        assert!(r.sessions.minter_keys().is_empty());
        r.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 2);
    }

    #[test]
    fn an_invalidation_during_a_start_keeps_that_minter_from_being_recorded() {
        let r = rig(Ok(PAGE));
        r.engine.lock().unwrap().start_sleep_ms = 400;
        let s = r.sessions.clone();
        let started = thread::spawn(move || s.get_pot(&req("a")));
        thread::sleep(Duration::from_millis(100));

        r.sessions.invalidate_caches();

        assert!(started.join().unwrap().is_ok());
        assert!(r.sessions.minter_keys().is_empty());
        r.engine.lock().unwrap().start_sleep_ms = 0;
        r.sessions.get_pot(&req("b")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 2);
    }

    #[test]
    fn an_empty_binding_takes_the_innertube_contexts_visitor_data() {
        let r = rig(Ok(PAGE));
        let mut by_context = req("");
        by_context.innertube_context = Some(json!({"client": {"visitorData": "VD1"}}));

        let a = r.sessions.get_pot(&by_context).unwrap();
        let none = r.sessions.get_pot(&req("")).err().unwrap();

        assert_eq!(a.content_binding, "VD1");
        assert_eq!(a.po_token, "tok1-VD1");
        assert!(none.contains("no content_binding"), "{none}");
    }

    #[test]
    fn a_malformed_request_challenge_names_the_step_and_leaves_no_minter() {
        let r = rig(Err("offline"));
        let mut bad = req("a");
        bad.challenge = Some(json!({"program": "p"}));

        let e = r.sessions.get_pot(&bad).err().unwrap();

        assert!(e.contains("challenge"), "{e}");
        assert!(r.sessions.minter_keys().is_empty());
        assert_eq!(r.engine.lock().unwrap().starts, 0);
    }
}
