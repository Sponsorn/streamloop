use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use serde_json::Value;

use super::{Api, ApiError};

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}

type Answer = Result<Value, ApiError>;

#[derive(Default)]
struct Script {
    standing: HashMap<(&'static str, String), Answer>,
    once: HashMap<(&'static str, String), VecDeque<Answer>>,
    texts: HashMap<String, Result<String, ApiError>>,
    requests: Vec<Request>,
}

/// A scripted `Api` for tests. An unscripted call answers `Unreachable`.
#[derive(Default)]
pub struct FakeApi {
    script: Mutex<Script>,
}

impl FakeApi {
    pub fn new() -> Arc<FakeApi> {
        Arc::new(FakeApi::default())
    }

    /// Sets the standing answer for a method and path (query string included).
    pub fn reply(&self, method: &'static str, path: &str, answer: Answer) {
        self.script
            .lock()
            .unwrap()
            .standing
            .insert((method, path.to_string()), answer);
    }

    /// Queues an answer used once, before the standing one.
    pub fn reply_once(&self, method: &'static str, path: &str, answer: Answer) {
        self.script
            .lock()
            .unwrap()
            .once
            .entry((method, path.to_string()))
            .or_default()
            .push_back(answer);
    }

    pub fn text(&self, path: &str, answer: Result<String, ApiError>) {
        self.script
            .lock()
            .unwrap()
            .texts
            .insert(path.to_string(), answer);
    }

    pub fn requests(&self) -> Vec<Request> {
        self.script.lock().unwrap().requests.clone()
    }

    pub fn posts(&self) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "POST")
            .collect()
    }

    fn answer(&self, method: &'static str, path: &str, body: Option<Value>) -> Answer {
        let mut s = self.script.lock().unwrap();
        s.requests.push(Request {
            method,
            path: path.to_string(),
            body,
        });
        let key = (method, path.to_string());
        if let Some(a) = s.once.get_mut(&key).and_then(VecDeque::pop_front) {
            return a;
        }
        s.standing.get(&key).cloned().unwrap_or(Err(ApiError::Unreachable))
    }
}

impl Api for FakeApi {
    fn get_json(&self, path: &str) -> Answer {
        self.answer("GET", path, None)
    }

    fn get_text(&self, path: &str) -> Result<String, ApiError> {
        let mut s = self.script.lock().unwrap();
        s.requests.push(Request {
            method: "GET",
            path: path.to_string(),
            body: None,
        });
        s.texts.get(path).cloned().unwrap_or(Err(ApiError::Unreachable))
    }

    fn post(&self, path: &str, body: Option<Value>) -> Answer {
        self.answer("POST", path, body)
    }

    fn forget_token(&self) {}
}
