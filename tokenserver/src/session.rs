// Port of bgutil-ytdlp-pot-provider 2.0.0's session_manager.ts minter and token cache.
// Copyright (c) Brainicism and contributors, GPL-3.0.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::engine::Engine;
use crate::google::Web;
use crate::homepage::{challenge_from_json, parse_challenge, parse_ytcfg, Challenge};

pub const TOKEN_TTL: Duration = Duration::from_secs(6 * 3600);

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
    tokens: HashMap<String, (String, SystemTime)>,
    /// Bumped by the invalidations, so a start that began before one does not outlive it.
    generation: u64,
}

pub struct Sessions {
    /// Held across a whole start, so requests that find no live minter wait on one start. A
    /// cached answer and `minter_keys` read `state` and never wait here.
    engine: Mutex<Box<dyn Engine>>,
    web: Box<dyn Web>,
    clock: Clock,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Sessions {
    pub fn new(engine: Box<dyn Engine>, web: Box<dyn Web>, clock: Clock) -> Self {
        Self { engine: Mutex::new(engine), web, clock, state: Mutex::new(State::default()) }
    }

    pub fn get_pot(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        let id = &r.content_binding;
        let result = self.get_pot_inner(r);
        match &result {
            Ok(_) => println!("get_pot {id}: ok"),
            Err(e) => println!("get_pot {id}: error {e}"),
        }
        result
    }

    /// Drops the minter and every cached token.
    pub fn invalidate_caches(&self) {
        let mut st = lock(&self.state);
        st.generation += 1;
        st.minter_expiry = None;
        st.tokens.clear();
    }

    /// The minter counts as expired; cached tokens stay.
    pub fn invalidate_it(&self) {
        let mut st = lock(&self.state);
        st.generation += 1;
        st.minter_expiry = None;
    }

    pub fn minter_keys(&self) -> Vec<String> {
        if self.minter_live() {
            vec!["default".to_owned()]
        } else {
            Vec::new()
        }
    }

    fn get_pot_inner(&self, r: &PotRequest) -> Result<PotAnswer, String> {
        let binding = &r.content_binding;
        if binding.is_empty() {
            return Err("content_binding is empty".to_owned());
        }
        if !r.bypass_cache {
            if let Some(hit) = self.cached(binding) {
                return Ok(hit);
            }
        }
        let mut engine = lock(&self.engine);
        // Another request may have built the minter, or cached this binding, while this one waited.
        if !r.bypass_cache {
            if let Some(hit) = self.cached(binding) {
                return Ok(hit);
            }
        }
        let generation = lock(&self.state).generation;
        if !self.minter_live() {
            match self.start(engine.as_mut(), r) {
                Ok(valid_for_s) => {
                    let mut st = lock(&self.state);
                    if st.generation == generation {
                        st.minter_expiry = Some((self.clock)() + Duration::from_secs(valid_for_s));
                    }
                }
                Err(e) => {
                    lock(&self.state).minter_expiry = None;
                    return Err(e);
                }
            }
        }
        let token = match engine.mint(binding) {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => return Err(self.mint_failed(binding, "empty token")),
            Err(e) => return Err(self.mint_failed(binding, &e)),
        };
        let expires_at = (self.clock)() + TOKEN_TTL;
        let mut st = lock(&self.state);
        if st.generation == generation {
            st.tokens.insert(binding.clone(), (token.clone(), expires_at));
        }
        Ok(PotAnswer { content_binding: binding.clone(), po_token: token, expires_at })
    }

    /// A failed mint drops the minter, so the next request starts over.
    fn mint_failed(&self, binding: &str, why: &str) -> String {
        lock(&self.state).minter_expiry = None;
        format!("mint for {binding}: {why}")
    }

    fn cached(&self, binding: &str) -> Option<PotAnswer> {
        let now = (self.clock)();
        let mut st = lock(&self.state);
        st.tokens.retain(|_, (_, exp)| now < *exp);
        st.tokens.get(binding).map(|(t, exp)| PotAnswer {
            content_binding: binding.to_owned(),
            po_token: t.clone(),
            expires_at: *exp,
        })
    }

    fn minter_live(&self) -> bool {
        let now = (self.clock)();
        matches!(lock(&self.state).minter_expiry, Some(exp) if now < exp)
    }

    /// Fetches the challenge and its interpreter once and starts the engine on them.
    fn start(&self, engine: &mut dyn Engine, r: &PotRequest) -> Result<u64, String> {
        let (challenge, globals, source) = match self.homepage_pair() {
            Ok((c, ytcfg)) => (c, json!({"yt": {"config_": ytcfg}}), "homepage"),
            Err(why) => {
                println!("minter: homepage failed ({why}), using the plugin's challenge");
                match r.challenge.as_ref() {
                    Some(v) => {
                        let c = challenge_from_json(v).ok_or_else(|| {
                            "the request's challenge lacks program, globalName or interpreterUrl".to_owned()
                        })?;
                        (c, json!({}), "plugin")
                    }
                    None => {
                        let c = self
                            .web
                            .att_get(r.innertube_context.as_ref())
                            .map_err(|e| format!("att/get: {e}"))?;
                        (c, json!({}), "att/get")
                    }
                }
            }
        };
        let interpreter_js = self
            .web
            .interpreter(&challenge.interpreter_url)
            .map_err(|e| format!("interpreter: {e}"))?;
        let valid_for_s = engine
            .start(&challenge, &interpreter_js, &globals)
            .map_err(|e| format!("BotGuard start: {e}"))?;
        println!("minter: {source} challenge, valid {valid_for_s} s");
        Ok(valid_for_s)
    }

    fn homepage_pair(&self) -> Result<(Challenge, Value), String> {
        let html = self.web.homepage().map_err(|e| format!("homepage fetch: {e}"))?;
        let ytcfg = parse_ytcfg(&html).ok_or("no ytcfg on the homepage")?;
        let challenge = parse_challenge(&html).ok_or("no challenge on the homepage")?;
        Ok((challenge, ytcfg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::UNIX_EPOCH;

    #[derive(Default)]
    struct EngineLog {
        starts: usize,
        mints: usize,
        globals: Vec<Value>,
        programs: Vec<String>,
        fail_next_start: bool,
        start_sleep_ms: u64,
    }

    struct FakeEngine(Arc<Mutex<EngineLog>>);

    impl Engine for FakeEngine {
        fn start(&mut self, c: &Challenge, _js: &str, globals: &Value) -> Result<u64, String> {
            let sleep = {
                let mut l = self.0.lock().unwrap();
                l.starts += 1;
                l.globals.push(globals.clone());
                l.programs.push(c.program.clone());
                if l.fail_next_start {
                    l.fail_next_start = false;
                    return Err("boom".to_owned());
                }
                l.start_sleep_ms
            };
            thread::sleep(Duration::from_millis(sleep));
            Ok(43200)
        }
        fn mint(&mut self, binding: &str) -> Result<String, String> {
            let mut l = self.0.lock().unwrap();
            l.mints += 1;
            Ok(format!("tok{}-{binding}", l.mints))
        }
    }

    struct WebLog {
        homepage_html: Result<String, String>,
        homepages: AtomicUsize,
        att_gets: AtomicUsize,
    }

    struct FakeWeb(Arc<WebLog>);

    impl Web for FakeWeb {
        fn homepage(&self) -> Result<String, String> {
            self.0.homepages.fetch_add(1, Ordering::SeqCst);
            self.0.homepage_html.clone()
        }
        fn interpreter(&self, _url: &str) -> Result<String, String> {
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

    struct Rig {
        sessions: Arc<Sessions>,
        engine: Arc<Mutex<EngineLog>>,
        web: Arc<WebLog>,
        now: Arc<Mutex<SystemTime>>,
    }

    fn rig(page: Result<&str, &str>) -> Rig {
        let engine = Arc::new(Mutex::new(EngineLog::default()));
        let web = Arc::new(WebLog {
            homepage_html: page.map(str::to_owned).map_err(str::to_owned),
            homepages: AtomicUsize::new(0),
            att_gets: AtomicUsize::new(0),
        });
        let now = Arc::new(Mutex::new(UNIX_EPOCH + Duration::from_secs(1_000_000)));
        let clock_now = now.clone();
        let sessions = Arc::new(Sessions::new(
            Box::new(FakeEngine(engine.clone())),
            Box::new(FakeWeb(web.clone())),
            Arc::new(move || *clock_now.lock().unwrap()),
        ));
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
    fn keeps_the_minter_until_its_lifetime_runs_out() {
        let r = rig(Ok(PAGE));
        let bypass = |b: &str| PotRequest { bypass_cache: true, ..req(b) };

        r.sessions.get_pot(&bypass("a")).unwrap();
        advance(&r, 43199);
        r.sessions.get_pot(&bypass("b")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 1);
        assert_eq!(r.sessions.minter_keys(), vec!["default"]);

        advance(&r, 2);
        assert!(r.sessions.minter_keys().is_empty());
        r.sessions.get_pot(&bypass("c")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 2);
        assert_eq!(r.web.homepages.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn caches_a_token_per_content_binding_for_six_hours_unless_bypassed() {
        let r = rig(Ok(PAGE));

        let first = r.sessions.get_pot(&req("a")).unwrap();
        let again = r.sessions.get_pot(&req("a")).unwrap();
        let other = r.sessions.get_pot(&req("b")).unwrap();
        let bypassed = r.sessions.get_pot(&PotRequest { bypass_cache: true, ..req("a") }).unwrap();

        assert_eq!(again.po_token, first.po_token);
        assert_eq!(again.expires_at, first.expires_at);
        assert_ne!(other.po_token, first.po_token);
        assert_ne!(bypassed.po_token, first.po_token);
        assert_eq!(r.engine.lock().unwrap().mints, 3);

        advance(&r, 6 * 3600 - 1);
        let still = r.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(still.po_token, bypassed.po_token);
        advance(&r, 2);
        let fresh = r.sessions.get_pot(&req("a")).unwrap();
        assert_ne!(fresh.po_token, bypassed.po_token);
    }

    #[test]
    fn invalidate_it_rebuilds_the_minter_and_invalidate_caches_drops_tokens() {
        let r = rig(Ok(PAGE));
        let first = r.sessions.get_pot(&req("a")).unwrap();

        r.sessions.invalidate_it();
        assert!(r.sessions.minter_keys().is_empty());
        let kept = r.sessions.get_pot(&req("a")).unwrap();
        assert_eq!(kept.po_token, first.po_token);
        assert_eq!(r.engine.lock().unwrap().starts, 1);
        let other = r.sessions.get_pot(&req("b")).unwrap();
        assert_eq!(r.engine.lock().unwrap().starts, 2);

        r.sessions.invalidate_caches();
        assert!(r.sessions.minter_keys().is_empty());
        let rebuilt = r.sessions.get_pot(&req("a")).unwrap();
        assert_ne!(rebuilt.po_token, first.po_token);
        assert_ne!(rebuilt.po_token, other.po_token);
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
    fn a_cached_token_and_minter_keys_do_not_wait_behind_a_slow_start() {
        let r = rig(Ok(PAGE));
        r.sessions.get_pot(&req("cached")).unwrap();
        r.sessions.invalidate_it();
        r.engine.lock().unwrap().start_sleep_ms = 600;
        let s = r.sessions.clone();
        let slow = thread::spawn(move || s.get_pot(&req("new")));
        thread::sleep(Duration::from_millis(150));

        let t = std::time::Instant::now();
        let hit = r.sessions.get_pot(&req("cached")).unwrap();
        r.sessions.minter_keys();

        assert!(t.elapsed() < Duration::from_millis(300));
        assert_eq!(hit.po_token, "tok1-cached");
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
