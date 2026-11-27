use std::{fmt, time::Duration};

pub mod client;
#[cfg(test)]
pub mod fake;
pub mod http;
pub mod types;

#[derive(Clone, Debug, PartialEq)]
pub enum ApiError {
    Unreachable,
    Unreadable,
    Server(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Unreachable => f.write_str("Can't reach the server."),
            ApiError::Unreadable => f.write_str("The server sent an answer the app cannot read."),
            ApiError::Server(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for ApiError {}

/// Blocking calls to the dashboard API, made from a background thread.
pub trait Api: Send + Sync + 'static {
    fn get_json(&self, path: &str) -> Result<serde_json::Value, ApiError>;
    fn get_text(&self, path: &str) -> Result<String, ApiError>;
    fn post(
        &self,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ApiError>;
    /// Drops the cached API token so the next POST reads a fresh one.
    fn forget_token(&self);
}

/// How long one call may take. `path` may carry a query string.
pub fn timeout_for(method: &str, path: &str) -> Duration {
    let path = path.split('?').next().unwrap_or(path);
    let secs = match (method, path) {
        ("POST", "/api/yt-dlp/update") => 300,
        // An update check right after Node starts waits for it to hash the install's parts.
        ("POST", "/api/update/check") => 300,
        ("POST", "/api/setup-check/run")
        | ("GET", "/api/playlist/videos")
        | ("POST", "/api/stream-key")
        | ("POST", "/api/update/apply") => 120,
        ("POST", "/api/stream/stop") | ("POST", "/api/discord/test") => 30,
        _ => 10,
    };
    Duration::from_secs(secs)
}
