//! `/player/next`, `/player/prev`, `/player/restart`, `/player/jump`, and `/stream/stop` and `/stream/start`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Value};
use streamloop_common::api::ControlCmd;

use super::update::refuse_during_update;
use super::{error_reply, json_reply, AppState, JsonBody};
use crate::launcher::LauncherState;

/// A video number within a list: a whole number from 0 to 999,999.
pub(super) fn video_index(v: &Value) -> Option<u32> {
    let n = v.as_f64()?;
    (n.fract() == 0.0 && (0.0..1_000_000.0).contains(&n)).then_some(n as u32)
}

/// A YouTube video id: 1 to 64 letters, digits, `_` or `-`.
pub(super) fn video_id(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    ((1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).then(|| s.to_string())
}

/// Why the stream cannot take a command now.
pub(super) fn not_ready(state: &AppState) -> Option<Response> {
    let why = if state.launcher.is_stopped_by_user() {
        "The stream is stopped. Start it first."
    } else if state.launcher.state() != LauncherState::Running {
        "The supervisor is not running."
    } else {
        return None;
    };
    Some(error_reply(StatusCode::SERVICE_UNAVAILABLE, why))
}

pub(super) fn refused_by_supervisor(error: Option<String>) -> Response {
    error_reply(StatusCode::BAD_GATEWAY, &format!("The supervisor refused: {}", error.as_deref().unwrap_or("unknown error")))
}

fn ok() -> Response {
    json_reply(StatusCode::OK, &json!({ "ok": true }))
}

async fn control(state: &AppState, cmd: ControlCmd, index: Option<u32>, id: Option<String>) -> Response {
    if let Some(why) = not_ready(state) {
        return why;
    }
    let r = state.client.control(cmd, index, None, id).await;
    if !r.ok {
        return refused_by_supervisor(r.error);
    }
    ok()
}

async fn simple(state: &AppState, cmd: ControlCmd) -> Response {
    match refuse_during_update(state) {
        Some(refused) => refused,
        None => control(state, cmd, None, None).await,
    }
}

pub(super) async fn next(State(state): State<Arc<AppState>>) -> Response {
    simple(&state, ControlCmd::Next).await
}

pub(super) async fn prev(State(state): State<Arc<AppState>>) -> Response {
    simple(&state, ControlCmd::Prev).await
}

pub(super) async fn restart(State(state): State<Arc<AppState>>) -> Response {
    simple(&state, ControlCmd::Restart).await
}

pub(super) async fn jump(State(state): State<Arc<AppState>>, Extension(JsonBody(body)): Extension<JsonBody>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    let Some(index) = body.get("index").and_then(video_index) else {
        return error_reply(StatusCode::BAD_REQUEST, "Invalid video index");
    };
    let id = match body.get("id") {
        None => None,
        Some(v) => match video_id(v) {
            Some(id) => Some(id),
            None => return error_reply(StatusCode::BAD_REQUEST, "Invalid video id"),
        },
    };
    control(&state, ControlCmd::Jump, Some(index), id).await
}

pub(super) async fn stop(State(state): State<Arc<AppState>>) -> Response {
    // A confirmation lets Stop through, as the way out of a new version that airs badly; a landing does not.
    if !state.updates.confirming() {
        if let Some(refused) = refuse_during_update(&state) {
            return refused;
        }
    }
    // Mark first, so the launcher does not relaunch while the supervisor shuts down.
    state.launcher.mark_stopped_by_user();
    let answered = state.client.shutdown().await;
    tracing::info!(answered, "Stream stopped from the dashboard");
    ok()
}

pub(super) async fn start(State(state): State<Arc<AppState>>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    state.launcher.launch_now();
    tracing::info!("Stream started from the dashboard");
    ok()
}
