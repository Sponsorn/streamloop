//! `/logs` and `/logs/:source/:name`: the supervisor's `data\logs` and the control process's `app\logs`.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use streamloop_common::time::iso;

use super::{error_reply, typed_reply, AppState};

const LOG_TAIL_BYTES: u64 = 1024 * 1024;
const NOT_FOUND: &str = "Log file not found";

/// `/^[A-Za-z0-9_.-]+\.(log|jsonl|csv)$/`.
fn log_name(name: &str) -> bool {
    let named = [".log", ".jsonl", ".csv"].iter().any(|ext| name.len() > ext.len() && name.ends_with(ext));
    named && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The sources in the order `/logs` lists them before sorting.
fn sources(state: &AppState) -> [(&'static str, PathBuf); 2] {
    [("supervisor", state.layout.data.join("logs")), ("node", state.layout.app_logs.clone())]
}

#[derive(Serialize)]
struct LogFile {
    name: String,
    size: u64,
    mtime: String,
    source: &'static str,
}

/// `stat.mtime.toISOString()`: the modification time to the nearest millisecond.
fn mtime_iso(meta: &std::fs::Metadata) -> String {
    let ms = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| (d.as_nanos() as f64 / 1e6).round() as u64);
    iso(ms)
}

pub(super) async fn list(State(state): State<Arc<AppState>>) -> Response {
    let mut files = Vec::new();
    for (source, dir) in sources(&state) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !log_name(&name) {
                continue;
            }
            if let Some(meta) = std::fs::metadata(entry.path()).ok().filter(|m| m.is_file()) {
                files.push(LogFile { name, size: meta.len(), mtime: mtime_iso(&meta), source });
            }
        }
    }
    // Newest first; the sort is stable, so equal times keep the source order.
    files.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    typed_reply(StatusCode::OK, &files)
}

/// The whole file, or its last 1 MB after a note, as UTF-8 with any broken sequence replaced.
fn read_tail(path: &std::path::Path, size: u64) -> std::io::Result<String> {
    if size <= LOG_TAIL_BYTES {
        return Ok(String::from_utf8_lossy(&std::fs::read(path)?).into_owned());
    }
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(size - LOG_TAIL_BYTES))?;
    let mut buf = vec![0; LOG_TAIL_BYTES as usize];
    f.read_exact(&mut buf)?;
    Ok(format!("... (truncated, showing last 1 MB) ...\n{}", String::from_utf8_lossy(&buf)))
}

pub(super) async fn read(State(state): State<Arc<AppState>>, Path((source, name)): Path<(String, String)>) -> Response {
    let Some((_, dir)) = sources(&state).into_iter().find(|(s, _)| *s == source) else {
        return error_reply(StatusCode::BAD_REQUEST, "Unknown log source");
    };
    if !log_name(&name) {
        return error_reply(StatusCode::BAD_REQUEST, "Invalid log file name");
    }
    let full = dir.join(&name);
    // A folder, or a device name like con.log, passes the name check and can block a read.
    let Some(meta) = std::fs::metadata(&full).ok().filter(|m| m.is_file()) else {
        return error_reply(StatusCode::NOT_FOUND, NOT_FOUND);
    };
    match read_tail(&full, meta.len()) {
        Ok(text) => {
            let mut res = (StatusCode::OK, text).into_response();
            res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
            res
        }
        Err(_) => error_reply(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read log file"),
    }
}
