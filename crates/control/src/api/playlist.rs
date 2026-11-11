//! `GET /playlist/videos` (one page of a configured list, through the cache) and `POST /playlist/switch`.

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde::Serialize;
use serde_json::{json, Value};
use streamloop_common::api::ReloadRequest;

use super::player::{not_ready, refused_by_supervisor, video_id, video_index};
use super::update::refuse_during_update;
use super::{error_reply, is_js_space, json_reply, typed_reply, AppState, JsonBody};
use crate::playlists::PlaylistVideo;

const INVALID_PLAYLIST: &str = "Invalid playlist index";

/// A query parameter as Express's parser hands it over: absent, one string, or something else
/// (repeated, or written with brackets).
enum Param {
    Missing,
    One(String),
    Other,
}

fn param(pairs: &[(String, String)], key: &str) -> Param {
    let mut found = pairs.iter().filter(|(k, _)| k == key || k.strip_prefix(key).is_some_and(|rest| rest.starts_with('[')));
    match (found.next(), found.next()) {
        (None, _) => Param::Missing,
        (Some((k, v)), None) if k == key => Param::One(v.clone()),
        _ => Param::Other,
    }
}

/// JavaScript's `Number(text)`: NaN for anything that is not a number literal, 0 for blank text.
fn js_number(text: &str) -> f64 {
    let s = text.trim_matches(is_js_space);
    if s.is_empty() {
        return 0.0;
    }
    match s {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(digits) = s.strip_prefix(prefix) {
            return u128::from_str_radix(digits, radix).map_or(f64::NAN, |n| n as f64);
        }
    }
    if s.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E')) {
        s.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

/// `Number(param) || fallback`.
fn number_or(p: Param, fallback: f64) -> f64 {
    let n = match p {
        Param::One(v) => js_number(&v),
        _ => f64::NAN,
    };
    if n.is_nan() || n == 0.0 {
        fallback
    } else {
        n
    }
}

/// A number as `JSON.stringify` writes it: whole numbers without a fraction, null when not finite.
fn js_json(n: f64) -> Value {
    if !n.is_finite() {
        Value::Null
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// `Array.prototype.slice` for a non-negative `start` and `end`.
fn slice(videos: &[PlaylistVideo], start: f64, end: f64) -> &[PlaylistVideo] {
    let at = |x: f64| if x.is_nan() { 0 } else { x.trunc().clamp(0.0, videos.len() as f64) as usize };
    let (from, to) = (at(start), at(end));
    if to > from {
        &videos[from..to]
    } else {
        &[]
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<'a> {
    videos: &'a [PlaylistVideo],
    total: usize,
    page: Value,
    per_page: Value,
    current_index: Option<usize>,
    playing: bool,
}

pub(super) async fn videos(State(state): State<Arc<AppState>>, query: Result<Query<Vec<(String, String)>>, QueryRejection>) -> Response {
    let pairs = query.map(|Query(p)| p).unwrap_or_default();
    let page = number_or(param(&pairs, "page"), 1.0).max(1.0);
    let per_page = number_or(param(&pairs, "perPage"), 25.0).clamp(1.0, 100.0);
    let playlists = state.config().playlists;
    let which = match param(&pairs, "playlist") {
        Param::Missing => Some(0),
        Param::One(raw) if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) => Some(raw.parse::<usize>().unwrap_or(usize::MAX)),
        _ => None,
    };
    let Some(list) = which.and_then(|i| playlists.get(i)) else {
        return error_reply(StatusCode::BAD_REQUEST, INVALID_PLAYLIST);
    };
    let (metadata, sup) = tokio::join!(state.playlists.fetch(&list.id), state.client.status());
    let metadata = match metadata {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(%err, "Failed to fetch playlist metadata");
            return error_reply(StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch playlist metadata");
        }
    };
    let start = (page - 1.0) * per_page;
    let now = sup.and_then(|s| s.playing);
    // A supervisor that names no list plays the config's first.
    let on_air = now.as_ref().and_then(|p| p.playlist_id.clone().flatten()).filter(|id| !id.is_empty());
    let playing = match on_air {
        Some(id) => list.id == id,
        None => which == Some(0),
    };
    let reply = Page {
        videos: slice(&metadata.videos, start, start + per_page),
        total: metadata.videos.len(),
        page: js_json(page),
        per_page: js_json(per_page),
        current_index: if playing { now.and_then(|p| p.index) } else { None },
        playing,
    };
    typed_reply(StatusCode::OK, &reply)
}

pub(super) async fn switch(State(state): State<Arc<AppState>>, Extension(JsonBody(body)): Extension<JsonBody>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    let playlists = state.config().playlists;
    let index = body.get("playlistIndex").and_then(Value::as_f64).filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n < playlists.len() as f64);
    let Some(index) = index else {
        return error_reply(StatusCode::BAD_REQUEST, INVALID_PLAYLIST);
    };
    let start_index = match body.get("videoIndex") {
        None => None,
        Some(v) => match video_index(v) {
            Some(i) => Some(i as usize),
            None => return error_reply(StatusCode::BAD_REQUEST, "Invalid video index"),
        },
    };
    let start_id = match body.get("videoId") {
        None => None,
        Some(v) => match video_id(v) {
            Some(id) => Some(id),
            None => return error_reply(StatusCode::BAD_REQUEST, "Invalid video id"),
        },
    };
    if let Some(why) = not_ready(&state) {
        return why;
    }
    // The supervisor keeps the list on air itself, so the configured order stays as the user set it.
    let sup = state.client.status().await;
    if sup.and_then(|s| s.playing).is_some_and(|p| p.playlist_id.is_none()) {
        return error_reply(StatusCode::CONFLICT, "Switching playlists needs a newer supervisor. Install the full ZIP.");
    }
    let req = ReloadRequest { playlist: Some(playlists[index as usize].id.clone()), index: start_index, id: start_id };
    let r = state.client.reload(req).await;
    if !r.ok {
        return refused_by_supervisor(r.error);
    }
    json_reply(StatusCode::OK, &json!({ "ok": true }))
}
