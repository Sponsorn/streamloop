//! `/status`, `/state` and `/events`, and the stream state the dashboard shows.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use serde_json::{json, Value};
use streamloop_common::api::SupervisorStatus;
use streamloop_common::config::is_first_run;

use super::{typed_reply, AppState};
use crate::launcher::LauncherState;
use crate::twitch::TwitchStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamState {
    Live,
    Recovering,
    OffAir,
    Starting,
    Stopped,
}

impl StreamState {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamState::Live => "live",
            StreamState::Recovering => "recovering",
            StreamState::OffAir => "off-air",
            StreamState::Starting => "starting",
            StreamState::Stopped => "stopped",
        }
    }
}

/// The at-a-glance answer to "is it working", with one plain sentence when it is not. An empty
/// `wait` or `block` counts as none, as in JavaScript.
pub fn stream_state(sup: Option<&SupervisorStatus>, launcher: LauncherState, stopped: bool, wait: Option<&str>, block: Option<&str>) -> (StreamState, Option<String>) {
    let said = |s: &str| Some(s.to_string());
    // The old supervisor can answer for a moment after Stop, so the user's stop wins.
    if stopped {
        return (StreamState::Stopped, said("The stream was stopped from the dashboard. It stays stopped, even after a restart, until you press Start."));
    }
    // So does a Start that waits for that old supervisor to exit.
    if let Some(wait) = wait.filter(|w| !w.is_empty()) {
        return (StreamState::Starting, said(wait));
    }
    let Some(sup) = sup else {
        if launcher == LauncherState::Ready {
            return (StreamState::Stopped, said("Ready to stream. Press Start."));
        }
        if let Some(block) = block.filter(|b| !b.is_empty()) {
            return (StreamState::OffAir, said(block));
        }
        return match launcher {
            LauncherState::Starting => (StreamState::Starting, said("The supervisor is starting.")),
            LauncherState::Missing => (StreamState::OffAir, said("The supervisor program was not found in the install folder.")),
            _ => (StreamState::OffAir, said("The supervisor is not answering.")),
        };
    };
    let alive = sup.encoder.as_ref().and_then(|e| e.alive) == Some(true);
    match sup.state.as_deref() {
        Some("starting") => (StreamState::Starting, said("The stream is starting.")),
        Some("needs-stream-key") => (StreamState::OffAir, said("The stream key is missing or cannot be read. Set it again in Settings.")),
        Some("waiting-for-playlist") => (StreamState::OffAir, said("The playlist could not be loaded.")),
        Some("stopping") => (StreamState::OffAir, said("The supervisor is shutting down.")),
        Some("recovering") => (StreamState::Recovering, said(if alive { "Recovering the current video." } else { "The encoder is restarting." })),
        Some("playing") => {
            if !alive {
                return (StreamState::OffAir, said("The encoder is not running."));
            }
            if matches!(sup.hold, Some(Some(_))) {
                return (StreamState::Recovering, said("Every video failed, so the slate airs until the next try."));
            }
            let step = sup.recovery.as_ref().and_then(|r| r.step.as_deref()).unwrap_or("");
            if !step.is_empty() && step != "none" {
                return (StreamState::Recovering, said("Recovering the current video."));
            }
            (StreamState::Live, None)
        }
        other => (StreamState::OffAir, Some(format!("The supervisor reports an unknown state: {}.", other.unwrap_or("undefined")))),
    }
}

#[derive(Serialize)]
struct Stream {
    state: StreamState,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReply {
    stream: Stream,
    supervisor: Option<SupervisorStatus>,
    launcher: &'static str,
    twitch: TwitchStatus,
    first_run: bool,
    system_memory: Value,
    uptime_ms: u64,
}

const GIB: f64 = 1_073_741_824.0;

/// `+(bytes / GiB).toFixed(1)`.
fn gb(bytes: f64) -> f64 {
    ((bytes / GIB) * 10.0).round() / 10.0
}

/// Physical memory as Node's `os.totalmem()` and `os.freemem()` report it.
fn system_memory() -> Value {
    let (total, free) = crate::discord::system_memory();
    let (total, free) = (total as f64, free as f64);
    let used = total - free;
    // Math.round, and NaN (written as null) for a total of 0.
    let percent = if total > 0.0 { json!((used / total * 100.0 + 0.5).floor() as u64) } else { Value::Null };
    json!({ "totalGB": gb(total), "freeGB": gb(free), "usedGB": gb(used), "usedPercent": percent })
}

pub(super) async fn status(State(state): State<Arc<AppState>>) -> Response {
    let sup = state.client.status().await;
    let l = &state.launcher;
    let launcher = l.state();
    let (stream_state, reason) = stream_state(sup.as_ref(), launcher, l.is_stopped_by_user(), l.wait_reason().as_deref(), l.block_reason().as_deref());
    let reply = StatusReply {
        stream: Stream { state: stream_state, reason },
        supervisor: sup,
        launcher: launcher.as_str(),
        twitch: (state.twitch)(),
        first_run: is_first_run(&state.config()),
        system_memory: system_memory(),
        uptime_ms: (state.uptime_ms)(),
    };
    typed_reply(StatusCode::OK, &reply)
}

#[derive(Serialize)]
struct NowPlaying {
    index: Option<usize>,
    id: Option<String>,
    title: Option<String>,
    position: Option<f64>,
    duration: Option<f64>,
    source: Option<String>,
}

pub(super) async fn now_playing(State(state): State<Arc<AppState>>) -> Response {
    let p = state.client.status().await.and_then(|s| s.playing).unwrap_or_default();
    let reply = NowPlaying {
        index: p.index,
        id: p.id.flatten(),
        title: p.title.flatten(),
        position: p.position,
        duration: p.duration.flatten(),
        source: p.source,
    };
    typed_reply(StatusCode::OK, &reply)
}

pub(super) async fn events(State(state): State<Arc<AppState>>) -> Response {
    typed_reply(StatusCode::OK, &state.timeline.entries())
}
