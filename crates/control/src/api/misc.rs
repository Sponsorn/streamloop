//! `/discord/test`, `/discord/defaults`, `/yt-dlp/update`, `/test/exit-75` and `/shutdown`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use serde_json::json;
use streamloop_common::config::ordered::Value as Ordered;
use streamloop_common::config::{DEFAULT_TEMPLATES, TEMPLATE_VARIABLES};

use super::config::ordered_reply;
use super::update::refuse_during_update;
use super::{error_reply, json_reply, text_reply, typed_reply, AppState};
use crate::discord::Level;
use crate::ytdlp_update::update_ytdlp as run_update;

const SHUTDOWN_DELAY: Duration = Duration::from_millis(500);

pub(super) async fn discord_test(State(state): State<Arc<AppState>>) -> Response {
    if state.config().discord.webhook_url.is_empty() {
        return error_reply(StatusCode::BAD_REQUEST, "No Discord webhook URL configured");
    }
    (state.notifier)().send("Test notification from StreamLoop. If you see this, your webhook is working!", Level::Info, None).await;
    json_reply(StatusCode::OK, &json!({ "ok": true }))
}

pub(super) async fn discord_defaults() -> Response {
    let templates = DEFAULT_TEMPLATES.iter().map(|(k, t)| (k.to_string(), Ordered::String(t.to_string()))).collect();
    let variables = TEMPLATE_VARIABLES
        .iter()
        .map(|(k, vars)| (k.to_string(), Ordered::Array(vars.iter().map(|v| Ordered::String(v.to_string())).collect())))
        .collect();
    let reply = Ordered::Object(vec![("templates".into(), Ordered::Object(templates)), ("variables".into(), Ordered::Object(variables))]);
    ordered_reply(StatusCode::OK, &reply)
}

#[derive(Serialize)]
struct Updated {
    ok: bool,
    version: Option<String>,
}

pub(super) async fn update_ytdlp(State(state): State<Arc<AppState>>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    let result = run_update(&*state.runner, &state.layout.ytdlp, |p| p.exists()).await;
    if result.ok {
        // A newer yt-dlp may list more of a playlist, so the cached listings go.
        state.playlists.clear();
        tracing::info!(version = ?result.version, "yt-dlp updated");
        typed_reply(StatusCode::OK, &Updated { ok: true, version: result.version })
    } else {
        tracing::error!(error = ?result.error, "Failed to update yt-dlp");
        error_reply(StatusCode::INTERNAL_SERVER_ERROR, result.error.as_deref().unwrap_or("Failed to update yt-dlp"))
    }
}

/// `STREAMLOOP_TEST_HOOKS=1` only: the update restart without an update.
pub(super) async fn exit_75(State(state): State<Arc<AppState>>) -> Response {
    tracing::warn!("Test hook: exiting with code 75 as after an update");
    (state.trigger_restart)();
    json_reply(StatusCode::OK, &json!({ "ok": true }))
}

/// Answers first; the control process stops half a second later, so the reply gets out.
pub(super) async fn shutdown(State(state): State<Arc<AppState>>) -> Response {
    let stop = state.trigger_shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(SHUTDOWN_DELAY).await;
        stop();
    });
    text_reply(StatusCode::OK, r#"{"ok":true,"message":"Shutting down..."}"#.into())
}
