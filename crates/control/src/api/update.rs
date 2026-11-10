//! `/update/status`, `/update/check`, `/update/apply`, and the 409 every changing route gives while an update lands.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use super::{error_reply, text_reply, typed_reply, AppState};
use crate::landing::{self, read_update_state, LandingDeps, UpdateResult, UpdateState};
use crate::updater::{UpdateInfo, UpdateStatus};

pub(super) const UPDATE_RUNNING: &str = "An update is running. Try again when it is done.";

/// What the routes ask of the updater and the landing.
#[async_trait]
pub trait Updates: Send + Sync {
    fn status(&self) -> UpdateInfo;
    async fn check(&self) -> UpdateInfo;
    /// From the landing's first step until the process exits, or until the landing fails.
    fn landing_in_progress(&self) -> bool;
    /// While this process confirms a landed update.
    fn confirming(&self) -> bool;
    /// Stages first when nothing is staged. On `Ok` the caller exits 75.
    async fn land(&self) -> Result<(), String>;
}

/// The exe updater and its landing.
pub struct ExeUpdates {
    pub landing: Arc<LandingDeps>,
}

#[async_trait]
impl Updates for ExeUpdates {
    fn status(&self) -> UpdateInfo {
        self.landing.updater.status()
    }

    async fn check(&self) -> UpdateInfo {
        self.landing.updater.check().await
    }

    fn landing_in_progress(&self) -> bool {
        self.landing.landing_in_progress()
    }

    fn confirming(&self) -> bool {
        false
    }

    async fn land(&self) -> Result<(), String> {
        landing::land(&self.landing).await
    }
}

/// While an update lands, or the version it landed is still being confirmed. An exe staging in the
/// background is not an update running: the stream and its controls are untouched until the landing.
fn update_running(state: &AppState) -> bool {
    state.updates.landing_in_progress() || read_update_state(&state.layout.update_state).is_some_and(|s| s.result == UpdateResult::Running)
}

/// The 409 for Stop, Start, controls and saves, which would race the stop, the swap or the confirmation.
pub(super) fn refuse_during_update(state: &AppState) -> Option<Response> {
    update_running(state).then(|| error_reply(StatusCode::CONFLICT, UPDATE_RUNNING))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReply {
    #[serde(flatten)]
    info: UpdateInfo,
    progress: Option<UpdateState>,
    /// Master's part swaps; an exe update swaps none.
    swap_log: [(); 0],
}

pub(super) async fn status(State(state): State<Arc<AppState>>) -> Response {
    let reply = StatusReply { info: state.updates.status(), progress: read_update_state(&state.layout.update_state), swap_log: [] };
    typed_reply(StatusCode::OK, &reply)
}

/// The updater keeps the 60 s cooldown and answers its status as it is during a check or a stage.
pub(super) async fn check(State(state): State<Arc<AppState>>) -> Response {
    typed_reply(StatusCode::OK, &state.updates.check().await)
}

/// Update Now: answers at once and lands in the background; the dashboard follows `/update/status`.
pub(super) async fn apply(State(state): State<Arc<AppState>>) -> Response {
    let info = state.updates.status();
    // An apply during a check would install from the offer the check is replacing.
    if info.status == UpdateStatus::Checking {
        return error_reply(StatusCode::CONFLICT, UPDATE_RUNNING);
    }
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    if info.is_dev_mode {
        return error_reply(StatusCode::INTERNAL_SERVER_ERROR, "Cannot apply updates in dev mode");
    }
    if !info.update_available {
        return error_reply(StatusCode::INTERNAL_SERVER_ERROR, "No update available to apply");
    }
    let (updates, restart) = (state.updates.clone(), state.trigger_restart.clone());
    tokio::spawn(async move {
        match updates.land().await {
            Ok(()) => restart(),
            Err(err) => tracing::error!(%err, "Failed to apply update"),
        }
    });
    text_reply(StatusCode::OK, r#"{"ok":true,"message":"Update started"}"#.into())
}
