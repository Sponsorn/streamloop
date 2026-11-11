//! Outbound HTTP behind a trait: Discord, Helix, GQL, GitHub and downloads go through it, and tests answer from `FakeHttp`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

#[derive(Debug, Clone, Default)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
}

impl HttpRequest {
    pub fn get(url: &str) -> Self {
        Self { method: "GET".into(), url: url.into(), ..Self::default() }
    }

    pub fn post_json(url: &str, body: &serde_json::Value) -> Self {
        Self {
            method: "POST".into(),
            url: url.into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: Some(body.to_string().into_bytes()),
            timeout: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self { status, headers: Vec::new(), body: body.into() }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpErrorKind {
    Timeout,
    /// `code` is the Node-style socket code (`ECONNREFUSED`, `ENOTFOUND`, ...).
    Connect { code: String },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub kind: HttpErrorKind,
    pub message: String,
}

impl HttpError {
    pub fn timeout(message: impl Into<String>) -> Self {
        Self { kind: HttpErrorKind::Timeout, message: message.into() }
    }

    pub fn connect(code: &str, message: impl Into<String>) -> Self {
        Self { kind: HttpErrorKind::Connect { code: code.into() }, message: message.into() }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self { kind: HttpErrorKind::Other, message: message.into() }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

#[async_trait]
pub trait Http: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
    /// Streams the body of a GET to `dest` and returns the byte count. A non-2xx answer is an error and writes nothing.
    /// `DOWNLOAD_STALL` with nothing from the server, before the answer or between chunks, is a `Timeout` error.
    async fn download(&self, url: &str, dest: &Path) -> Result<u64, HttpError>;

    /// `download` that calls `progress(received, total)` after each chunk; `total` is the content-length when the server sent one.
    async fn download_with_progress(&self, url: &str, dest: &Path, progress: &Progress<'_>) -> Result<u64, HttpError> {
        let _ = progress;
        self.download(url, dest).await
    }
}

/// Bytes received so far, and the expected total when known.
pub type Progress<'a> = dyn Fn(u64, Option<u64>) + Send + Sync + 'a;

/// A download that gets nothing from the server for this long fails, so a dead connection cannot hold it forever.
pub const DOWNLOAD_STALL: Duration = Duration::from_secs(60);

fn stalled(after: Duration) -> HttpError {
    HttpError::timeout(format!("no data arrived for {} seconds", after.as_secs()))
}

#[async_trait]
trait Chunks: Send {
    /// The next piece of the body, `None` at its end.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError>;
}

#[async_trait]
impl Chunks for reqwest::Response {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        self.chunk().await.map(|c| c.map(|b| b.to_vec())).map_err(map_error)
    }
}

async fn write_body(body: &mut dyn Chunks, dest: &Path, expected: Option<u64>, progress: &Progress<'_>, stall: Duration) -> Result<u64, HttpError> {
    use tokio::io::AsyncWriteExt;
    let io = |e: std::io::Error| HttpError::other(e.to_string());
    let mut file = tokio::fs::File::create(dest).await.map_err(io)?;
    let mut total = 0u64;
    loop {
        let chunk = tokio::time::timeout(stall, body.next_chunk()).await.map_err(|_| stalled(stall))??;
        let Some(chunk) = chunk else { break };
        file.write_all(&chunk).await.map_err(io)?;
        total += chunk.len() as u64;
        progress(total, expected);
    }
    file.flush().await.map_err(io)?;
    Ok(total)
}

pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new() -> Self {
        Self { client: reqwest::Client::builder().build().expect("the TLS backend initialises") }
    }

    /// Follows a redirect only to a URL `allow` accepts, at most 10 hops; any other redirect is an error.
    pub fn following_only(allow: impl Fn(&reqwest::Url) -> bool + Send + Sync + 'static) -> Self {
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if allow(attempt.url()) {
                attempt.follow()
            } else {
                let refused = format!("refused a redirect to {}", attempt.url());
                attempt.error(refused)
            }
        });
        Self { client: reqwest::Client::builder().redirect(policy).build().expect("the TLS backend initialises") }
    }
}

impl Default for ReqwestHttp {
    fn default() -> Self {
        Self::new()
    }
}

fn io_code(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind::*;
    match kind {
        ConnectionRefused => "ECONNREFUSED",
        ConnectionReset => "ECONNRESET",
        ConnectionAborted => "ECONNABORTED",
        TimedOut => "ETIMEDOUT",
        NotFound => "ENOTFOUND",
        _ => "ECONNECT",
    }
}

fn map_error(e: reqwest::Error) -> HttpError {
    use std::error::Error;
    if e.is_timeout() {
        return HttpError::timeout(e.to_string());
    }
    if e.is_connect() {
        let mut source = e.source();
        let mut code = "ECONNECT";
        while let Some(s) = source {
            if let Some(io) = s.downcast_ref::<std::io::Error>() {
                code = io_code(io.kind());
                break;
            }
            if s.to_string().to_ascii_lowercase().contains("dns") {
                code = "ENOTFOUND";
            }
            source = s.source();
        }
        return HttpError::connect(code, e.to_string());
    }
    // A redirect policy's own refusal says more than reqwest's "error following redirect".
    if let Some(source) = e.is_redirect().then(|| e.source()).flatten() {
        return HttpError::other(source.to_string());
    }
    HttpError::other(e.to_string())
}

#[async_trait]
impl Http for ReqwestHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes()).map_err(|e| HttpError::other(e.to_string()))?;
        let mut builder = self.client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        if let Some(t) = req.timeout {
            builder = builder.timeout(t);
        }
        let res = builder.send().await.map_err(map_error)?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
            .collect();
        let body = res.bytes().await.map_err(map_error)?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<u64, HttpError> {
        self.download_with_progress(url, dest, &|_, _| {}).await
    }

    async fn download_with_progress(&self, url: &str, dest: &Path, progress: &Progress<'_>) -> Result<u64, HttpError> {
        let answer = tokio::time::timeout(DOWNLOAD_STALL, self.client.get(url).send()).await.map_err(|_| stalled(DOWNLOAD_STALL))?;
        let mut res = answer.map_err(map_error)?;
        if !res.status().is_success() {
            return Err(HttpError::other(format!("HTTP {}", res.status().as_u16())));
        }
        let expected = res.content_length();
        write_body(&mut res, dest, expected, progress, DOWNLOAD_STALL).await
    }
}

/// Sends `first`, then nothing more, the way a dead connection behaves.
struct StalledBody {
    first: Option<Vec<u8>>,
}

#[async_trait]
impl Chunks for StalledBody {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        match self.first.take() {
            Some(first) => Ok(Some(first)),
            None => std::future::pending().await,
        }
    }
}

type Handler = Arc<dyn Fn(&HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync>;

/// Answers by URL prefix (the longest match wins) and records every request, downloads included.
#[derive(Default)]
pub struct FakeHttp {
    routes: Mutex<Vec<(String, Handler)>>,
    stalls: Mutex<Vec<(String, Vec<u8>)>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeHttp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn route(&self, prefix: &str, handler: impl Fn(&HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync + 'static) {
        self.routes.lock().unwrap().push((prefix.to_string(), Arc::new(handler)));
    }

    /// Always answers `status` and `body` for URLs under `prefix`.
    pub fn respond(&self, prefix: &str, status: u16, body: &str) {
        let body = body.to_string();
        self.route(prefix, move |_| Ok(HttpResponse::new(status, body.clone())));
    }

    /// The next download under `prefix` gets `first` and then nothing, until `DOWNLOAD_STALL` ends it.
    pub fn stall_next(&self, prefix: &str, first: &[u8]) {
        self.stalls.lock().unwrap().push((prefix.to_string(), first.to_vec()));
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn answer(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let handler = {
            let routes = self.routes.lock().unwrap();
            routes.iter().filter(|(p, _)| req.url.starts_with(p.as_str())).max_by_key(|(p, _)| p.len()).map(|(_, h)| h.clone())
        };
        let result = match handler {
            Some(h) => h(&req),
            None => Err(HttpError::other(format!("FakeHttp: no route for {}", req.url))),
        };
        self.requests.lock().unwrap().push(req);
        result
    }
}

#[async_trait]
impl Http for FakeHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.answer(req)
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<u64, HttpError> {
        self.download_with_progress(url, dest, &|_, _| {}).await
    }

    /// The body arrives as one chunk; `total` is the route's content-length header, if it set one.
    async fn download_with_progress(&self, url: &str, dest: &Path, progress: &Progress<'_>) -> Result<u64, HttpError> {
        let stalled = {
            let mut stalls = self.stalls.lock().unwrap();
            stalls.iter().position(|(p, _)| url.starts_with(p.as_str())).map(|i| stalls.remove(i).1)
        };
        if let Some(first) = stalled {
            self.requests.lock().unwrap().push(HttpRequest::get(url));
            return write_body(&mut StalledBody { first: Some(first) }, dest, None, progress, DOWNLOAD_STALL).await;
        }
        let res = self.answer(HttpRequest::get(url))?;
        if !(200..300).contains(&res.status) {
            return Err(HttpError::other(format!("HTTP {}", res.status)));
        }
        tokio::fs::write(dest, &res.body).await.map_err(|e| HttpError::other(e.to_string()))?;
        let len = res.body.len() as u64;
        progress(len, res.header("content-length").and_then(|v| v.parse().ok()));
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_http_prefers_the_longest_matching_prefix_and_records_requests() {
        // Arrange
        let http = FakeHttp::new();
        http.respond("https://api.example.com/", 200, "general");
        http.respond("https://api.example.com/users", 201, "specific");
        // Act
        let res = http.send(HttpRequest::get("https://api.example.com/users/1")).await.unwrap();
        let other = http.send(HttpRequest::get("https://api.example.com/x")).await.unwrap();
        // Assert
        assert_eq!((res.status, res.text()), (201, "specific".to_string()));
        assert_eq!(other.text(), "general");
        assert_eq!(http.requests().len(), 2);
    }

    #[tokio::test]
    async fn fake_http_fails_for_a_url_with_no_route() {
        // Arrange
        let http = FakeHttp::new();
        // Act
        let err = http.send(HttpRequest::get("https://nowhere.example/")).await.unwrap_err();
        // Assert
        assert_eq!(err.kind, HttpErrorKind::Other);
    }

    #[tokio::test]
    async fn fake_download_writes_the_body_and_returns_its_length() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.zip");
        let http = FakeHttp::new();
        http.respond("https://dl.example/", 200, "12345");
        // Act
        let n = http.download("https://dl.example/a.zip", &dest).await.unwrap();
        // Assert
        assert_eq!(n, 5);
        assert_eq!(std::fs::read(&dest).unwrap(), b"12345");
    }

    #[tokio::test]
    async fn fake_download_reports_the_body_as_one_chunk_with_its_content_length() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.exe");
        let http = FakeHttp::new();
        http.route("https://dl.example/", |_| {
            let mut res = HttpResponse::new(200, "1234");
            res.headers.push(("content-length".into(), "4".into()));
            Ok(res)
        });
        let seen = Mutex::new(Vec::new());
        // Act
        http.download_with_progress("https://dl.example/a.exe", &dest, &|got, total| seen.lock().unwrap().push((got, total))).await.unwrap();
        // Assert
        assert_eq!(*seen.lock().unwrap(), vec![(4, Some(4))]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_stops_coming_fails_after_the_stall_time() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let http = FakeHttp::new();
        http.stall_next("https://dl.example/", b"half");
        let started = tokio::time::Instant::now();
        // Act
        let err = http.download("https://dl.example/a.exe", &dir.path().join("a.exe")).await.unwrap_err();
        // Assert
        assert_eq!(err, HttpError::timeout("no data arrived for 60 seconds"));
        assert!(started.elapsed() >= DOWNLOAD_STALL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_fails_the_download_after_the_stall_time() {
        // Arrange: accepts the connection and then says nothing
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/a.exe", listener.local_addr().unwrap());
        let silent = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let dir = tempfile::tempdir().unwrap();
        let started = tokio::time::Instant::now();
        // Act
        let err = ReqwestHttp::new().download(&url, &dir.path().join("a.exe")).await.unwrap_err();
        // Assert
        assert_eq!(err, HttpError::timeout("no data arrived for 60 seconds"));
        assert!(started.elapsed() >= DOWNLOAD_STALL);
        silent.abort();
    }
}
