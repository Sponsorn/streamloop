use std::sync::Mutex;

use serde_json::Value;

use super::{Api, ApiError, timeout_for};

const BAD_TOKEN: &str = "Forbidden: invalid API token";

pub struct HttpApi {
    base: String,
    agent: ureq::Agent,
    token: Mutex<Option<String>>,
}

impl HttpApi {
    pub fn new(port: u16) -> Self {
        HttpApi {
            base: format!("http://127.0.0.1:{port}"),
            agent: ureq::AgentBuilder::new().build(),
            token: Mutex::new(None),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The cached token, read from `GET /api/token` when there is none.
    fn token(&self) -> Result<String, ApiError> {
        if let Some(t) = self.token.lock().unwrap().clone() {
            return Ok(t);
        }
        let answer = self.get_json("/api/token")?;
        let t = answer
            .get("token")
            .and_then(Value::as_str)
            .ok_or(ApiError::Unreadable)?
            .to_string();
        *self.token.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    fn send_post(&self, path: &str, body: Option<&Value>) -> Result<String, ApiError> {
        let token = self.token()?;
        let req = self
            .agent
            .post(&self.url(path))
            .timeout(timeout_for("POST", path))
            .set("X-API-Token", &token);
        let result = match body {
            Some(b) => req
                .set("Content-Type", "application/json")
                .send_string(&b.to_string()),
            None => req.call(),
        };
        read_text(result)
    }
}

/// The body of a 2xx answer, or the error the dashboard would show.
fn read_text(result: Result<ureq::Response, ureq::Error>) -> Result<String, ApiError> {
    match result {
        Ok(resp) => resp.into_string().map_err(|_| ApiError::Unreadable),
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp
                .into_string()
                .ok()
                .and_then(|b| serde_json::from_str::<Value>(&b).ok())
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string));
            Err(ApiError::Server(
                text.unwrap_or_else(|| format!("HTTP {code}")),
            ))
        }
        Err(ureq::Error::Transport(_)) => Err(ApiError::Unreachable),
    }
}

fn parse(text: &str) -> Result<Value, ApiError> {
    serde_json::from_str(text).map_err(|_| ApiError::Unreadable)
}

impl Api for HttpApi {
    fn get_json(&self, path: &str) -> Result<Value, ApiError> {
        parse(&self.get_text(path)?)
    }

    fn get_text(&self, path: &str) -> Result<String, ApiError> {
        let req = self
            .agent
            .get(&self.url(path))
            .timeout(timeout_for("GET", path));
        read_text(req.call())
    }

    fn post(&self, path: &str, body: Option<Value>) -> Result<Value, ApiError> {
        let mut result = self.send_post(path, body.as_ref());
        if result == Err(ApiError::Server(BAD_TOKEN.into())) {
            self.forget_token();
            result = self.send_post(path, body.as_ref());
        }
        parse(&result?)
    }

    fn forget_token(&self) {
        *self.token.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread::JoinHandle,
        time::Duration,
    };

    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        path: String,
        token: Option<String>,
        body: String,
    }

    struct Server {
        port: u16,
        seen: Arc<Mutex<Vec<Seen>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl Server {
        /// Answers one connection per scripted `(status, body)`, then ends.
        fn start(script: Vec<(u16, &'static str)>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            let thread = std::thread::spawn(move || {
                for (status, body) in script {
                    let (mut sock, _) = listener.accept().unwrap();
                    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    let head_end = loop {
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break p + 4;
                        }
                        let n = sock.read(&mut chunk).unwrap();
                        assert!(n > 0, "client closed early");
                        buf.extend_from_slice(&chunk[..n]);
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap().split(' ');
                    let method = first.next().unwrap().to_string();
                    let path = first.next().unwrap().to_string();
                    let mut token = None;
                    let mut len = 0usize;
                    for l in lines {
                        let lower = l.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("x-api-token:") {
                            token = Some(v.trim().to_string());
                        }
                        if let Some(v) = lower.strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap();
                        }
                    }
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut chunk).unwrap();
                        assert!(n > 0, "client closed early");
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body_in =
                        String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string();
                    log.lock().unwrap().push(Seen { method, path, token, body: body_in });
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    sock.write_all(reply.as_bytes()).unwrap();
                }
            });
            Server { port, seen, thread: Some(thread) }
        }

        fn finish(mut self) -> Vec<Seen> {
            self.thread.take().unwrap().join().unwrap();
            let seen = self.seen.lock().unwrap().clone();
            seen
        }
    }

    const FORBIDDEN: &str = r#"{"error":"Forbidden: invalid API token"}"#;

    #[test]
    fn sends_the_token_on_every_post() {
        let s = Server::start(vec![
            (200, r#"{"token":"t1"}"#),
            (200, r#"{"ok":true}"#),
            (200, r#"{"ok":true}"#),
        ]);
        let api = HttpApi::new(s.port);

        let a = api.post("/api/player/next", None);
        let b = api.post("/api/stream-key", Some(json!({"key": "k"})));

        assert_eq!(a, Ok(json!({"ok": true})));
        assert_eq!(b, Ok(json!({"ok": true})));
        let seen = s.finish();
        assert_eq!(seen[0].path, "/api/token");
        assert_eq!(seen[1].token.as_deref(), Some("t1"));
        assert_eq!(seen[2].token.as_deref(), Some("t1"));
        assert_eq!(seen[2].body, r#"{"key":"k"}"#);
    }

    #[test]
    fn reads_a_new_token_and_resends_once_after_a_403() {
        let s = Server::start(vec![
            (200, r#"{"token":"t1"}"#),
            (403, FORBIDDEN),
            (200, r#"{"token":"t2"}"#),
            (200, r#"{"ok":true}"#),
        ]);
        let api = HttpApi::new(s.port);

        let r = api.post("/api/player/next", None);

        assert_eq!(r, Ok(json!({"ok": true})));
        let seen = s.finish();
        let posts: Vec<_> = seen.iter().filter(|x| x.method == "POST").collect();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].token.as_deref(), Some("t1"));
        assert_eq!(posts[1].token.as_deref(), Some("t2"));
    }

    #[test]
    fn gives_up_after_a_second_403() {
        let s = Server::start(vec![
            (200, r#"{"token":"t1"}"#),
            (403, FORBIDDEN),
            (200, r#"{"token":"t2"}"#),
            (403, FORBIDDEN),
        ]);
        let api = HttpApi::new(s.port);

        let r = api.post("/api/player/next", None);

        assert_eq!(r, Err(ApiError::Server("Forbidden: invalid API token".into())));
        let seen = s.finish();
        assert_eq!(seen.iter().filter(|x| x.method == "POST").count(), 2);
    }

    #[test]
    fn uses_the_servers_error_text() {
        let s = Server::start(vec![
            (400, r#"{"error":"Invalid playlist index"}"#),
            (500, "<html>oops</html>"),
        ]);
        let api = HttpApi::new(s.port);

        let a = api.get_json("/api/playlist/videos?playlist=9");
        let b = api.get_json("/api/status");

        assert_eq!(a, Err(ApiError::Server("Invalid playlist index".into())));
        assert_eq!(b, Err(ApiError::Server("HTTP 500".into())));
        s.finish();
    }

    #[test]
    fn reports_a_refused_connection_as_unreachable() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let api = HttpApi::new(port);

        let r = api.get_json("/api/status");

        assert_eq!(r, Err(ApiError::Unreachable));
        assert_eq!(r.unwrap_err().to_string(), "Can't reach the server.");
    }

    #[test]
    fn reports_a_broken_answer_as_unreadable() {
        let s = Server::start(vec![(200, "{not json")]);
        let api = HttpApi::new(s.port);

        let r = api.get_json("/api/status");

        assert_eq!(r, Err(ApiError::Unreadable));
        assert_eq!(
            r.unwrap_err().to_string(),
            "The server sent an answer the app cannot read."
        );
        s.finish();
    }

    #[test]
    fn reads_plain_text_logs() {
        let s = Server::start(vec![(200, "line one\nline two")]);
        let api = HttpApi::new(s.port);

        let r = api.get_text("/api/logs/node/a.log");

        assert_eq!(r, Ok("line one\nline two".to_string()));
        s.finish();
    }

    #[test]
    fn knows_the_long_calls() {
        let s = Duration::from_secs;
        assert_eq!(timeout_for("POST", "/api/setup-check/run"), s(120));
        assert_eq!(timeout_for("POST", "/api/stream-key"), s(120));
        assert_eq!(
            timeout_for("GET", "/api/playlist/videos?playlist=0&page=1&perPage=25"),
            s(120)
        );
        assert_eq!(timeout_for("POST", "/api/update/apply"), s(120));
        assert_eq!(timeout_for("POST", "/api/yt-dlp/update"), s(300));
        assert_eq!(timeout_for("POST", "/api/update/check"), s(300));
        assert_eq!(timeout_for("POST", "/api/stream/stop"), s(30));
        assert_eq!(timeout_for("POST", "/api/discord/test"), s(30));
        assert_eq!(timeout_for("GET", "/api/status"), s(10));
    }
}
