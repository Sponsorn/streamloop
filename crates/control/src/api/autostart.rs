//! `/autostart`: the `StreamLoop` value under the registry's Run key, which starts one install's
//! `streamloop.exe ui --tray` at logon (see `autostart_store`). Turning it on in one install moves it there;
//! turning it off leaves another install's alone. R3's `StreamLoop.vbs` in the Startup folder is read only to
//! recognise one StreamLoop wrote, so turning autostart on removes it and the start-up migration replaces it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde::Serialize;
use serde_json::Value;

use super::{error_reply, is_js_space, js_truthy, text_reply, typed_reply, AppState, JsonBody};
use crate::autostart_store::{disabled, run_command, run_target, same_path};

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup`.
pub fn startup_folder() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    appdata.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Startup")
}

fn vbs(state: &AppState) -> PathBuf {
    state.startup_dir.join("StreamLoop.vbs")
}

fn this_bat(state: &AppState) -> PathBuf {
    state.layout.root.join("START.bat")
}

/// UTF-16LE or UTF-8 with a BOM; else BOM-less UTF-8 as 2.x writes it, else ANSI read byte for byte.
pub(crate) fn decode_vbs(raw: &[u8]) -> String {
    if let Some(rest) = raw.strip_prefix(&[0xff, 0xfe]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = raw.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_string(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    }
}

/// What `/WshShell\.Run\s+"""(.+?)"""/i` captures: the quoted path after the first `WshShell.Run`
/// that has one on the same line.
pub(crate) fn vbs_target(text: &str) -> Option<String> {
    const NEEDLE: &str = "wshshell.run";
    let mut from = 0;
    while let Some(at) = text[from..].char_indices().map(|(i, _)| from + i).find(|&i| text[i..].get(..NEEDLE.len()).is_some_and(|s| s.eq_ignore_ascii_case(NEEDLE))) {
        from = at + NEEDLE.len();
        let rest = &text[from..];
        let spaces = rest.len() - rest.trim_start_matches(is_js_space).len();
        if spaces == 0 {
            continue;
        }
        let Some(quoted) = rest[spaces..].strip_prefix("\"\"\"") else { continue };
        let line_end = quoted.find(['\n', '\r', '\u{2028}', '\u{2029}']).unwrap_or(quoted.len());
        // `.+?`: at least one character before the closing quotes.
        let first = quoted[..line_end].chars().next().map_or(0, char::len_utf8);
        if first == 0 {
            continue;
        }
        if let Some(end) = quoted[first..line_end].find("\"\"\"") {
            return Some(quoted[..first + end].to_string());
        }
    }
    None
}

/// The START.bat a `StreamLoop.vbs` in the Startup folder starts: `None` when there is no such file,
/// `Some(None)` when it runs nothing StreamLoop's text would.
fn vbs_start_bat(state: &AppState) -> std::io::Result<Option<Option<String>>> {
    let path = vbs(state);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(vbs_target(&decode_vbs(&std::fs::read(path)?))))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Autostart {
    enabled: bool,
    target: Option<String>,
    this_install: bool,
}

fn read_entry(state: &AppState) -> std::io::Result<Autostart> {
    let value = state.autostart.run_value()?;
    let approved = state.autostart.approved()?;
    let target = value.as_deref().and_then(run_target);
    let this_install = target.as_deref().is_some_and(|t| same_path(t, &state.layout.exe));
    Ok(Autostart { enabled: value.is_some() && !disabled(approved.as_deref()), target: target.map(|t| t.to_string_lossy().into_owned()), this_install })
}

pub(super) async fn read(State(state): State<Arc<AppState>>) -> Response {
    match read_entry(&state) {
        Ok(entry) => typed_reply(StatusCode::OK, &entry),
        Err(err) => {
            tracing::error!(%err, "Failed to read autostart");
            error_reply(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string())
        }
    }
}

/// What turning autostart off found that it must not touch.
enum Refusal {
    Conflict(String),
    Failed(std::io::Error),
}

impl From<std::io::Error> for Refusal {
    fn from(err: std::io::Error) -> Self {
        Refusal::Failed(err)
    }
}

fn turn_on(state: &AppState) -> std::io::Result<()> {
    state.autostart.set_run_value(&run_command(&state.layout))?;
    state.autostart.clear_approved()?;
    // One start remains: a VBS StreamLoop wrote, of any install, goes.
    if let Some(Some(_)) = vbs_start_bat(state)? {
        std::fs::remove_file(vbs(state))?;
    }
    tracing::info!("Autostart entry written");
    Ok(())
}

fn turn_off(state: &AppState) -> Result<(), Refusal> {
    let another = |target: &str| Refusal::Conflict(format!("Autostart points to another StreamLoop install: {target}."));
    let value = state.autostart.run_value()?;
    if let Some(value) = &value {
        if let Some(target) = run_target(value) {
            if !same_path(&target, &state.layout.exe) {
                return Err(another(&target.to_string_lossy()));
            }
        }
        state.autostart.delete_run_value()?;
        tracing::info!("Autostart entry removed");
    }
    match vbs_start_bat(state)? {
        Some(Some(target)) if same_path(Path::new(&target), &this_bat(state)) => {
            std::fs::remove_file(vbs(state))?;
            tracing::info!("Autostart shortcut removed");
        }
        // Only the old script is left to speak for autostart, and it is not this install's to remove.
        Some(Some(target)) if value.is_none() => return Err(another(&target)),
        Some(None) if value.is_none() => {
            return Err(Refusal::Conflict("StreamLoop.vbs in the Startup folder was not written by StreamLoop. Remove it from the Startup folder by hand.".into()));
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn set(State(state): State<Arc<AppState>>, Extension(JsonBody(body)): Extension<JsonBody>) -> Response {
    let enabled = body.get("enabled");
    let result = if enabled.is_some_and(js_truthy) { turn_on(&state).map_err(Refusal::Failed) } else { turn_off(&state) };
    match result {
        // `enabled` goes back as it came, and is left out when it was missing, as `res.json` does with undefined.
        Ok(()) => match enabled {
            Some(v) => text_reply(StatusCode::OK, format!("{{\"ok\":true,\"enabled\":{}}}", Value::to_string(v))),
            None => text_reply(StatusCode::OK, "{\"ok\":true}".into()),
        },
        Err(Refusal::Conflict(text)) => error_reply(StatusCode::CONFLICT, &text),
        Err(Refusal::Failed(err)) => {
            tracing::error!(%err, "Failed to set autostart");
            error_reply(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string())
        }
    }
}
