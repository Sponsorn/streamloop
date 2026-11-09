//! `POST /stream-key`: protects the key in this process and saves only the blob. The key is never
//! echoed, logged or written anywhere else.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Value};

use super::config::{js_trim, save_and_reload};
use super::update::refuse_during_update;
use super::{error_reply, text_reply, AppState, JsonBody};

pub(super) async fn set(State(state): State<Arc<AppState>>, Extension(JsonBody(body)): Extension<JsonBody>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    let key = match body.get("key") {
        Some(Value::String(k)) if !js_trim(k).is_empty() => k.as_str(),
        _ => return error_reply(StatusCode::BAD_REQUEST, "No stream key given"),
    };
    // Any error text could quote the key; this keeps it out of the reply and the log.
    let redact = |s: String| s.replace(js_trim(key), "[redacted]");
    let saved = match (state.protect)(key) {
        Ok(blob) => save_and_reload(&state, &json!({ "supervisor": { "streamKeyProtected": blob } })).await.map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    match saved {
        Ok(saved) => {
            tracing::info!(reload_error = ?saved.reload_error, "Stream key replaced");
            text_reply(StatusCode::OK, r#"{"ok":true,"streamKeySet":true}"#.into())
        }
        Err(e) => {
            let why = redact(e);
            tracing::error!(error = %why, "Failed to set the stream key");
            error_reply(StatusCode::INTERNAL_SERVER_ERROR, &why)
        }
    }
}
