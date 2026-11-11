//! `/setup-check` (the stored report) and `/setup-check/run`. Recommendations are not applied here;
//! the wizard saves them through `POST /config`.

use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde_json::Value;
use streamloop_common::paths::Layout;
use tokio::sync::watch;

use super::update::refuse_during_update;
use super::{error_reply, text_reply, typed_reply, AppState, JsonBody, RunSetupCheck};
use crate::runner::Runner;
use crate::setup_check::{run_setup_check, SetupReport};

const MIB: u64 = 1024 * 1024;

/// Free space for this user on the drive that holds `dir`, in whole MiB.
#[cfg(windows)]
pub fn free_mb(dir: &Path) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    if !dir.exists() {
        return Err(format!("{} does not exist", dir.display()));
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is a NUL-terminated path that outlives the call; the two totals it does not need are null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok == 0 {
        return Err(format!("Could not read the free space of {}: {}", dir.display(), std::io::Error::last_os_error()));
    }
    Ok(available / MIB)
}

#[cfg(not(windows))]
pub fn free_mb(dir: &Path) -> Result<u64, String> {
    Err(format!("Free space is only measured on Windows ({})", dir.display()))
}

/// The setup check as the control process runs it: free space on `data\` (or the root before
/// `data\` exists), then the encoder probes through `runner`, then the report in `data\setup-check.json`.
pub fn setup_check_with(runner: Arc<dyn Runner>) -> RunSetupCheck {
    Arc::new(move |layout: Layout, durations: Vec<f64>| {
        let runner = runner.clone();
        Box::pin(async move {
            let dir = if layout.data.exists() { &layout.data } else { &layout.root };
            let free = free_mb(dir)?;
            Ok(run_setup_check(&layout, &*runner, free, &durations).await)
        })
    })
}

pub(super) async fn report(State(state): State<Arc<AppState>>) -> Response {
    // The file as it is, so a report from another version keeps its fields.
    let stored = std::fs::read_to_string(&state.layout.setup_report).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    text_reply(StatusCode::OK, stored.unwrap_or(Value::Null).to_string())
}

/// A playlist id as the wizard sends it: 2 to 64 letters, digits, `_` or `-`.
fn playlist_id(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    ((2..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).then(|| s.to_string())
}

type Outcome = Option<Result<SetupReport, String>>;

/// Clears the running check however its task ends, so a later click starts a new one.
struct RunSlot(Arc<AppState>);

impl Drop for RunSlot {
    fn drop(&mut self) {
        *self.0.setup_run.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(super) async fn run(State(state): State<Arc<AppState>>, Extension(JsonBody(body)): Extension<JsonBody>) -> Response {
    if let Some(refused) = refuse_during_update(&state) {
        return refused;
    }
    // The wizard sends its playlist before it is saved; otherwise the one playing sizes the cache.
    let chosen = match body.get("playlistId") {
        None => None,
        Some(v) => match playlist_id(v) {
            Some(id) => Some(id),
            None => return error_reply(StatusCode::BAD_REQUEST, "Invalid playlist id"),
        },
    };
    let id = chosen.or_else(|| state.config().playlists.first().map(|p| p.id.clone()));
    let mut outcome = {
        let mut slot = state.setup_run.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            // A second click while the probes run gets the same result instead of a second set of probes.
            Some(rx) => rx.clone(),
            None => {
                let (tx, rx) = watch::channel::<Outcome>(None);
                *slot = Some(rx.clone());
                let running = RunSlot(state.clone());
                tokio::spawn(async move {
                    let st = &running.0;
                    let durations = match &id {
                        Some(id) => st.playlists.fetch(id).await.map(|m| m.videos.iter().map(|v| v.duration).collect()).unwrap_or_default(),
                        None => Vec::new(),
                    };
                    let result = (st.setup_check)(st.layout.clone(), durations).await;
                    drop(running);
                    let _ = tx.send(Some(result));
                });
                rx
            }
        }
    };
    let result = match outcome.wait_for(Option::is_some).await {
        Ok(r) => r.clone().expect("waited for a result"),
        Err(_) => Err("The setup check stopped before it finished.".to_string()),
    };
    match result {
        Ok(report) => typed_reply(StatusCode::OK, &report),
        Err(err) => {
            tracing::error!(%err, "Setup check failed");
            error_reply(StatusCode::INTERNAL_SERVER_ERROR, &err)
        }
    }
}
