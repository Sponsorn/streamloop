// Port of bgutil-ytdlp-pot-provider 2.0.0's session_manager.ts requests to YouTube and Google.
// Copyright (c) Brainicism and contributors, GPL-3.0.

use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE};
use reqwest::redirect::Policy;
use serde_json::{json, Value};

use crate::homepage::{challenge_from_json, interpreter_allowed, Challenge};

/// bgutils-js's USER_AGENT, which 2.0.0 sends on every request.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36(KHTML, like Gecko)";

const HOMEPAGE_URL: &str = "https://www.youtube.com";
const ATT_GET_URL: &str = "https://www.youtube.com/youtubei/v1/att/get?prettyPrint=false";
// YouTube's public web API key, the one bgutils-js sends.
const GOOG_API_KEY: &str = "AIzaSyDyT5W0Jh49F30Pqqtyfdf7pDLFKLJoAnw";
const TIMEOUT: Duration = Duration::from_secs(15);

pub trait Web: Send + Sync {
    fn homepage(&self) -> Result<String, String>;
    /// Refuses a URL `interpreter_allowed` rejects, before any request.
    fn interpreter(&self, url: &str) -> Result<String, String>;
    fn att_get(&self, innertube_context: Option<&Value>) -> Result<Challenge, String>;
}

/// TLS verified, no proxy (environment proxies are ignored), USER_AGENT on every request.
pub struct GoogleWeb {
    client: Client,
    /// Follows no redirect, so an interpreter script only ever comes from the host checked.
    interpreter_client: Client,
}

impl GoogleWeb {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            client: client(Policy::default())?,
            interpreter_client: client(Policy::none())?,
        })
    }
}

fn client(redirects: Policy) -> Result<Client, String> {
    Client::builder()
        .use_rustls_tls()
        .https_only(true)
        .no_proxy()
        .user_agent(USER_AGENT)
        .connect_timeout(TIMEOUT)
        .timeout(TIMEOUT)
        .redirect(redirects)
        .build()
        .map_err(|e| format!("could not set up the HTTP client: {e}"))
}

impl Web for GoogleWeb {
    fn homepage(&self) -> Result<String, String> {
        let resp = self
            .client
            .get(HOMEPAGE_URL)
            .header(ACCEPT, "*/*")
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.7")
            .send();
        text_of(resp, "homepage")
    }

    fn interpreter(&self, url: &str) -> Result<String, String> {
        let url = interpreter_fetch_url(url)?;
        text_of(self.interpreter_client.get(url).send(), "interpreter script")
    }

    fn att_get(&self, innertube_context: Option<&Value>) -> Result<Challenge, String> {
        let resp = self
            .client
            .post(ATT_GET_URL)
            .header(CONTENT_TYPE, "application/json")
            .header("x-goog-api-key", GOOG_API_KEY)
            .header("x-user-agent", "grpc-web-javascript/0.1")
            .body(att_get_body(innertube_context).to_string())
            .send();
        let text = text_of(resp, "att/get")?;
        let answer: Value =
            serde_json::from_str(&text).map_err(|e| format!("att/get: not JSON: {e}"))?;
        answer
            .get("bgChallenge")
            .and_then(challenge_from_json)
            .ok_or_else(|| "att/get: no bgChallenge in the answer".to_owned())
    }
}

/// The body of a 2xx answer. Anything else, a redirect included, is an error.
fn text_of(resp: reqwest::Result<Response>, what: &str) -> Result<String, String> {
    let resp = resp.map_err(|e| format!("{what}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{what}: HTTP {status}"));
    }
    resp.text().map_err(|e| format!("{what}: {e}"))
}

/// The https URL for an interpreter script, or Err when its host is not allowed.
fn interpreter_fetch_url(url: &str) -> Result<String, String> {
    if !interpreter_allowed(url) {
        return Err(format!("refusing the interpreter script at {url}: not an allowed Google host"));
    }
    Ok(match url.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_owned(),
    })
}

fn att_get_body(innertube_context: Option<&Value>) -> Value {
    let context = innertube_context.cloned().unwrap_or_else(|| {
        json!({"client": {"clientName": "WEB", "clientVersion": "2.20260817.01.00"}})
    });
    json!({"context": context, "engagementType": "ENGAGEMENT_TYPE_UNBOUND"})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fetches_protocol_relative_interpreter_urls_over_https() {
        assert_eq!(
            interpreter_fetch_url("//www.google.com/js/th/abc.js").unwrap(),
            "https://www.google.com/js/th/abc.js"
        );
        assert_eq!(
            interpreter_fetch_url("https://www.google.com/js/th/abc.js").unwrap(),
            "https://www.google.com/js/th/abc.js"
        );
    }

    #[test]
    fn refuses_interpreter_scripts_from_other_hosts_without_a_request() {
        let web = GoogleWeb::new().unwrap();
        for url in [
            "//evil.example/x.js",
            "http://www.google.com/js/th/abc.js",
            "//www.google.com.evil.example/x.js",
            "//www.google.com@evil.example/x.js",
        ] {
            let err = web.interpreter(url).unwrap_err();
            assert!(err.contains("refusing"), "{url}: {err}");
        }
    }

    #[test]
    fn att_get_sends_the_callers_innertube_context_or_2_0_0s_default() {
        let default = att_get_body(None);
        assert_eq!(default["context"]["client"]["clientName"], "WEB");
        assert_eq!(default["context"]["client"]["clientVersion"], "2.20260817.01.00");
        assert_eq!(default["engagementType"], "ENGAGEMENT_TYPE_UNBOUND");

        let ctx = json!({"client": {"clientName": "MWEB", "visitorData": "v1"}});
        assert_eq!(att_get_body(Some(&ctx))["context"], ctx);
    }
}
