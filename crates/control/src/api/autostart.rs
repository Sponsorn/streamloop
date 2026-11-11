//! `/autostart`: `StreamLoop.vbs` in the Windows Startup folder, which starts one install's `START.bat`
//! at logon. Turning it on in one install moves it there; turning it off leaves another install's alone.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Extension;
use serde::Serialize;
use serde_json::Value;

use super::{error_reply, is_js_space, js_truthy, text_reply, typed_reply, AppState, JsonBody};

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
fn decode_vbs(raw: &[u8]) -> String {
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
fn run_target(text: &str) -> Option<String> {
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

/// Node's `path.resolve(p).toLowerCase()` on both sides.
fn same_bat(a: &str, b: &Path) -> bool {
    let norm = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().replace('/', "\\").to_lowercase();
    norm(Path::new(a)) == norm(b)
}

/// The target of the VBS, `None` when there is no VBS or it runs nothing this code wrote.
fn autostart_target(state: &AppState) -> std::io::Result<Option<String>> {
    let path = vbs(state);
    if !path.exists() {
        return Ok(None);
    }
    Ok(run_target(&decode_vbs(&std::fs::read(path)?)))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Autostart {
    enabled: bool,
    target: Option<String>,
    this_install: bool,
}

pub(super) async fn read(State(state): State<Arc<AppState>>) -> Response {
    match autostart_target(&state) {
        Ok(target) => {
            let this_install = target.as_deref().is_some_and(|t| same_bat(t, &this_bat(&state)));
            typed_reply(StatusCode::OK, &Autostart { enabled: vbs(&state).exists(), target, this_install })
        }
        Err(err) => {
            tracing::error!(%err, "Failed to read autostart");
            error_reply(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string())
        }
    }
}

/// UTF-16LE with a BOM: WScript reads a BOM-less UTF-8 file as ANSI and misses non-ASCII paths.
fn write_vbs(path: &Path, bat: &Path) -> std::io::Result<()> {
    let content = format!("Set WshShell = CreateObject(\"WScript.Shell\")\r\nWshShell.Run \"\"\"{}\"\"\", 1, False", bat.display());
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(content.encode_utf16().flat_map(u16::to_le_bytes));
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub(super) async fn set(State(state): State<Arc<AppState>>, Extension(JsonBody(body)): Extension<JsonBody>) -> Response {
    let enabled = body.get("enabled");
    let path = vbs(&state);
    let result = if enabled.is_some_and(js_truthy) {
        write_vbs(&path, &this_bat(&state)).map(|()| tracing::info!("Autostart shortcut created"))
    } else if path.exists() {
        match autostart_target(&state) {
            Ok(None) => {
                return error_reply(StatusCode::CONFLICT, "StreamLoop.vbs in the Startup folder was not written by StreamLoop. Remove it from the Startup folder by hand.");
            }
            Ok(Some(target)) if !same_bat(&target, &this_bat(&state)) => {
                return error_reply(StatusCode::CONFLICT, &format!("Autostart points to another StreamLoop install: {target}."));
            }
            Ok(Some(_)) => std::fs::remove_file(&path).map(|()| tracing::info!("Autostart shortcut removed")),
            Err(err) => Err(err),
        }
    } else {
        Ok(())
    };
    match result {
        // `enabled` goes back as it came, and is left out when it was missing, as `res.json` does with undefined.
        Ok(()) => match enabled {
            Some(v) => text_reply(StatusCode::OK, format!("{{\"ok\":true,\"enabled\":{}}}", Value::to_string(v))),
            None => text_reply(StatusCode::OK, "{\"ok\":true}".into()),
        },
        Err(err) => {
            tracing::error!(%err, "Failed to set autostart");
            error_reply(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string())
        }
    }
}
