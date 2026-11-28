//! `/cache`: the supervisor's cache report, passed on as it came, or null when none answers.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use streamloop_common::api::CacheReport;

use super::{typed_reply, AppState};

#[derive(Serialize)]
struct Reply {
    cache: Option<CacheReport>,
}

pub(super) async fn report(State(state): State<Arc<AppState>>) -> Response {
    typed_reply(StatusCode::OK, &Reply { cache: state.client.cache().await })
}
