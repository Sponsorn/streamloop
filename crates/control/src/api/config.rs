//! `GET /config` with the secrets masked, and `POST /config`, which saves, applies and asks the
//! supervisor to reload.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde_json::Value;
use streamloop_common::api::ReloadRequest;
use streamloop_common::config::ordered::Value as Ordered;
use streamloop_common::config::{to_json, Config, ConfigError};

use super::update::refuse_during_update;
use super::{error_reply, is_js_space, text_reply, AppState, JsonBody};

const MASK: &str = "********";

/// `JSON.stringify` of a value that keeps its key order.
pub(super) fn ordered_reply(status: StatusCode, body: &Ordered) -> Response {
    text_reply(status, body.to_compact())
}

fn field_mut<'a>(v: &'a mut Ordered, key: &str) -> Option<&'a mut Ordered> {
    match v {
        Ordered::Object(o) => o.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// The config as the dashboard reads it: secrets as `********` (or empty when unset), and only
/// whether a stream key is set, never its blob.
pub(super) fn masked(config: &Config) -> Ordered {
    let mask = |secret: &str| Ordered::String(if secret.is_empty() { "" } else { MASK }.into());
    let mut v = Ordered::parse(&to_json(config)).expect("to_json writes JSON");
    v.set("twitchClientSecret", mask(&config.twitch_client_secret));
    if let Some(discord) = field_mut(&mut v, "discord") {
        discord.set("webhookUrl", mask(&config.discord.webhook_url));
    }
    if let Some(supervisor) = field_mut(&mut v, "supervisor") {
        supervisor.remove("streamKeyProtected");
        supervisor.set("streamKeySet", Ordered::Bool(!config.supervisor.stream_key_protected.is_empty()));
    }
    v
}

/// JavaScript's `trim()`.
pub(super) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// The supervisor refuses a `config.json` with a bad cookie value, so a save must not write one.
/// Only the save checks: a hand-edited file must not stop the control process from starting.
fn cookie_setting_error(value: Option<&Value>) -> Option<&'static str> {
    let v = match value? {
        Value::String(s) => js_trim(s),
        _ => return Some("\"yt-dlp cookies from browser\" must be text."),
    };
    if v.chars().count() > 200 {
        return Some("\"yt-dlp cookies from browser\" is longer than 200 characters.");
    }
    // No control characters: the value goes to yt-dlp as one argument.
    if v.chars().any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')) {
        return Some("\"yt-dlp cookies from browser\" contains a control character.");
    }
    None
}

pub(super) struct Saved {
    pub config: Config,
    pub restart_required: bool,
    pub reload_error: Option<String>,
    pub reload_error_code: Option<String>,
}

/// Saves `patch`, takes the new config in and asks the supervisor to read it.
pub(super) async fn save_and_reload(state: &AppState, patch: &Value) -> Result<Saved, ConfigError> {
    let config = {
        // Held across the file write, so two saves never merge into the same old file.
        let mut current = state.config.write().unwrap_or_else(|e| e.into_inner());
        let saved = streamloop_common::config::save(patch, &state.config_path)?;
        *current = saved.clone();
        saved
    };
    (state.apply_config)(&config);
    let reload = state.client.reload(ReloadRequest::default()).await;
    let (reload_error, reload_error_code) = if reload.ok { (None, None) } else { (Some(reload.error.unwrap_or_else(|| "reload failed".into())), reload.code) };
    Ok(Saved { config, restart_required: reload.restart_required, reload_error, reload_error_code })
}

fn text_or_null(s: Option<String>) -> Ordered {
    s.map_or(Ordered::Null, Ordered::String)
}

pub(super) async fn read(State(state): State<Arc<AppState>>) -> Response {
    ordered_reply(StatusCode::OK, &masked(&state.config()))
}

pub(super) async fn save(State(state): State<Arc<AppState>>, Extension(JsonBody(mut body)): Extension<JsonBody>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    if let Value::Object(o) = &mut body {
        // The dashboard sends the masked secrets back unchanged.
        if o.get("twitchClientSecret").and_then(Value::as_str) == Some(MASK) {
            o.remove("twitchClientSecret");
        }
        if let Some(Value::Object(discord)) = o.get_mut("discord") {
            if discord.get("webhookUrl").and_then(Value::as_str) == Some(MASK) {
                discord.remove("webhookUrl");
            }
        }
        // Only /stream-key sets the key; streamKeySet is the masked read-back.
        if let Some(Value::Object(supervisor)) = o.get_mut("supervisor") {
            supervisor.remove("streamKeyProtected");
            supervisor.remove("streamKeySet");
            // An explicit preferredEncoder is the user's pick. Without one, a changed encoder becomes the preference;
            // resending the current one (any save after a fallback) must not make the fallback the preference.
            let explicit = matches!(supervisor.get("preferredEncoder"), Some(Value::String(_)));
            if let (false, Some(Value::String(encoder))) = (explicit, supervisor.get("encoder")) {
                if encoder != state.config().supervisor.encoder.as_str() {
                    supervisor.insert("preferredEncoder".into(), Value::String(encoder.clone()));
                }
            }
        }
    }
    if let Some(why) = cookie_setting_error(body.get("ytdlCookiesFromBrowser")) {
        return error_reply(StatusCode::BAD_REQUEST, why);
    }
    match save_and_reload(&state, &body).await {
        Ok(saved) => {
            tracing::info!(restart_required = saved.restart_required, reload_error = ?saved.reload_error, "Config updated via API");
            let reply = Ordered::Object(vec![
                ("ok".into(), Ordered::Bool(true)),
                ("config".into(), masked(&saved.config)),
                ("restartRequired".into(), Ordered::Bool(saved.restart_required)),
                ("reloadError".into(), text_or_null(saved.reload_error)),
                ("reloadErrorCode".into(), text_or_null(saved.reload_error_code)),
            ]);
            ordered_reply(StatusCode::OK, &reply)
        }
        Err(err) => {
            tracing::error!(%err, "Failed to save config");
            error_reply(StatusCode::BAD_REQUEST, &err.to_string())
        }
    }
}
