// One live check of the BotGuard engine: the YouTube homepage once, its interpreter script
// once, one BotGuard run with GenerateIT, one token for dQw4w9WgXcQ. Prints
// {"validForS", "interpreterHost", "tokenLength"} on stdout, never the token; on failure
// {"stage", "error"} and exit code 1. Stage timings go to stderr.
//
//   cargo run --release --example integrity [-- --snapshot DIR]

#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;
#[allow(dead_code)]
#[path = "../src/google.rs"]
mod google;
#[allow(dead_code)]
#[path = "../src/homepage.rs"]
mod homepage;

use std::path::PathBuf;
use std::time::Instant;

use serde_json::{json, Value};

use engine::{BotguardEngine, Engine};
use google::{GoogleWeb, Web, USER_AGENT};

const CONTENT_BINDING: &str = "dQw4w9WgXcQ";

fn main() {
    let snapshot = match snapshot_arg() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}\nusage: integrity [--snapshot DIR]");
            std::process::exit(2);
        }
    };
    match run(snapshot) {
        Ok(out) => println!("{out}"),
        Err((stage, error)) => {
            println!("{}", json!({"stage": stage, "error": error}));
            std::process::exit(1);
        }
    }
}

fn snapshot_arg() -> Result<Option<PathBuf>, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => Ok(None),
        [flag, dir] if flag == "--snapshot" => Ok(Some(PathBuf::from(dir))),
        _ => Err(format!("unexpected arguments: {args:?}")),
    }
}

fn run(snapshot: Option<PathBuf>) -> Result<Value, (&'static str, String)> {
    let web = GoogleWeb::new().map_err(|e| ("client", e))?;

    let t = Instant::now();
    let html = web.homepage().map_err(|e| ("homepage", e))?;
    eprintln!("homepage: {} bytes in {:.1} s", html.len(), t.elapsed().as_secs_f64());

    let ytcfg = homepage::parse_ytcfg(&html).ok_or(("ytcfg", "no ytcfg.set({...}) on the homepage".to_owned()))?;
    eprintln!("ytcfg: EVENT_ID present: {}", ytcfg.get("EVENT_ID").is_some());
    let challenge = homepage::parse_challenge(&html).ok_or_else(|| ("challenge", missing_challenge(&html)))?;
    eprintln!("challenge: {}", challenge_shape(&html));

    let t = Instant::now();
    let interpreter_js = web.interpreter(&challenge.interpreter_url).map_err(|e| ("interpreter", e))?;
    eprintln!("interpreter: {} bytes in {:.1} s", interpreter_js.len(), t.elapsed().as_secs_f64());

    let mut engine = BotguardEngine::new(USER_AGENT, snapshot.as_deref());
    let t = Instant::now();
    let valid_for_s = engine
        .start(&challenge, &interpreter_js, &json!({"yt": {"config_": ytcfg}}))
        .map_err(|e| ("start", e))?;
    eprintln!("start: {:.1} s", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let token = engine.mint(CONTENT_BINDING).map_err(|e| ("mint", e))?;
    eprintln!("mint: {:.2} s", t.elapsed().as_secs_f64());

    Ok(json!({
        "validForS": valid_for_s,
        "interpreterHost": host_of(&challenge.interpreter_url),
        "tokenLength": token.len(),
    }))
}

fn host_of(url: &str) -> &str {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("//")).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

/// Which reading of the ytAtN payload finds the challenge: 2.0.0's (loose JSON, under `R`)
/// or a strict JSON parse with `bgChallenge` at the top.
fn challenge_shape(html: &str) -> String {
    let payload = homepage::ytatn_payload(html);
    let loose = payload.and_then(homepage::parse_loose_json);
    let strict = payload.and_then(|p| serde_json::from_str::<Value>(p).ok());
    format!(
        "loose R.bgChallenge: {}, strict parse: {}, strict top-level bgChallenge: {}",
        loose.as_ref().is_some_and(|v| v.get("R").and_then(|r| r.get("bgChallenge")).is_some()),
        strict.is_some(),
        strict.as_ref().is_some_and(|v| v.get("bgChallenge").is_some()),
    )
}

/// What the page held where the challenge should be: marker count and key names only, so a
/// failed run can be read without fetching the page again.
fn missing_challenge(html: &str) -> String {
    let markers = html.matches("window.ytAtN(").count();
    let keys = |v: &Value| v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>());
    let payload = homepage::ytatn_payload(html).map(|p| match homepage::parse_loose_json(p) {
        Some(v) => format!("keys {:?}, R keys {:?}", keys(&v), v.get("R").and_then(keys)),
        None => format!("{} bytes that do not parse", p.len()),
    });
    format!("no BotGuard challenge: {markers} window.ytAtN( markers, payload: {payload:?}")
}
