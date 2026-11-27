//! The UI tests' window on a `FakeApi`: opening the app, reading what a window painted, and canned answers.

use std::{
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use gpui::{AnyWindowHandle, AppContext, ElementId, Entity, TestAppContext, VisualTestContext};
use gpui_kit::{base::test_support::texts, component::Root, test::TestWindowExt};
use serde_json::{Value, json};

use crate::{
    Shell,
    api::{client::Client, fake::FakeApi},
    app::AppView,
    start::StartedBat,
};

/// A shell on `fake` without a root, so the window state stays in memory.
pub fn shell(fake: Arc<FakeApi>) -> Shell {
    Shell {
        client: Client(fake),
        root: None,
        state_path: None,
    }
}

/// Opens the app window on `fake` and runs until it waits for a timer.
/// Drive it with `cx.update_window` and `TestWindowExt`; move time with `cx.executor().advance_clock`.
pub fn open_app(cx: &mut TestAppContext, fake: Arc<FakeApi>) -> (Entity<AppView>, AnyWindowHandle) {
    open_shell(cx, &shell(fake), None)
}

/// `open_app` for an app that started `START.bat` in `root` `ago` before its window opened, as `run` opens it.
pub fn open_app_started(
    cx: &mut TestAppContext,
    fake: Arc<FakeApi>,
    root: PathBuf,
    ago: Duration,
) -> (Entity<AppView>, AnyWindowHandle) {
    open_shell(cx, &shell(fake), Some((root, ago)))
}

/// `open_app` on `shell`, through `open_main_window` as `run` opens the window.
pub fn open_shell(
    cx: &mut TestAppContext,
    shell: &Shell,
    started_bat: Option<(PathBuf, Duration)>,
) -> (Entity<AppView>, AnyWindowHandle) {
    // Init gets its own update: in the window's update, its effects draw the new window a second time.
    cx.update(gpui_kit::init);
    cx.update(crate::theme::init);
    let now = cx.update(|cx| crate::app::now_ms(cx));
    let started_bat = started_bat.map(|(root, ago)| StartedBat {
        root,
        at_ms: now - ago.as_millis() as i64,
    });
    cx.update(|cx| crate::set_started_bat(cx, started_bat));
    let window = cx.update(|cx| crate::open_main_window(cx, shell));
    cx.run_until_parked();
    (app_view(cx, window), window)
}

/// The app's view inside `window`.
pub fn app_view(cx: &mut TestAppContext, window: AnyWindowHandle) -> Entity<AppView> {
    cx.update(|cx| {
        let root = window.downcast::<Root>().expect("the window holds a Root");
        let view = root.read(cx).expect("the window is open").view().clone();
        view.downcast::<AppView>().ok().expect("the Root holds the app")
    })
}

/// The window's close button: runs the close callback the window registered, as Windows' WM_CLOSE does.
pub fn close(cx: &mut TestAppContext, window: AnyWindowHandle) {
    let mut visual = VisualTestContext::from_window(window, cx);
    // `simulate_close` hands the callback back to the window after it ran, which panics with "window not
    // found" once the callback has removed the window. Only that panic is contained.
    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| visual.simulate_close())) {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if !message.contains("window not found") {
            resume_unwind(panic);
        }
    }
    cx.run_until_parked();
}

/// Draws a frame and returns the text it painted inside the window, in paint order.
pub fn painted(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<String> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        texts(window)
            .into_iter()
            .filter(|t| t.visible())
            .map(|t| t.text().to_string())
            .collect()
    })
    .expect("the window is open")
}

pub fn shows(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) -> bool {
    painted(cx, window).iter().any(|t| t == text)
}

/// The update banner's own text, then its Node notice, without the button's label; empty while
/// the banner is hidden.
pub fn banner_texts(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<String> {
    let (banner, button) = (
        ElementId::from("update-banner"),
        ElementId::from("update-btn"),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        texts(window)
            .into_iter()
            .filter(|t| t.visible() && t.path().contains(&banner) && !t.path().contains(&button))
            .map(|t| t.text().to_string())
            .collect()
    })
    .expect("the window is open")
}

/// The update banner's text; `None` while the banner is hidden.
pub fn banner_text(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<String> {
    banner_texts(cx, window).into_iter().next()
}

pub fn count(fake: &FakeApi, method: &str, path: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == method && r.path == path)
        .count()
}

/// Scripts the dashboard's poll: a live stream, three events, no update.
pub fn answer_polls(fake: &FakeApi) {
    fake.reply("GET", "/api/status", Ok(status_json("live", false)));
    fake.reply("GET", "/api/events", Ok(events_json(3)));
    fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
}

/// `GET /api/status` with the stream in `state`; the supervisor is gone when it is `stopped`.
pub fn status_json(state: &str, first_run: bool) -> Value {
    let supervisor = (state != "stopped").then(|| {
        json!({
            "playing": {
                "index": 2, "playlistId": "PLmain", "id": "vid-three", "title": "Third video",
                "source": "file", "position": 61.0, "duration": 3600.0
            },
            "encoder": { "alive": true, "speed": 1.0, "restarts": 0 },
            "youtube": { "breaker": "closed", "retryAt": null, "cookieProblem": null },
            "playlistCount": 10
        })
    });
    json!({
        "stream": { "state": state, "reason": null },
        "supervisor": supervisor,
        "launcher": if state == "stopped" { "stopped" } else { "running" },
        "twitch": { "enabled": false },
        "firstRun": first_run,
        "uptimeMs": 65_000
    })
}

/// `GET /api/config` as Node masks it: secrets as `********`, the key only as `streamKeySet`.
pub fn config_json() -> Value {
    json!({
        "playlists": [{ "id": "PLmain", "name": "Main" }, { "id": "PLsecond", "name": "Second" }],
        "port": 7654,
        "discord": {
            "webhookUrl": "********", "botName": "", "avatarUrl": "", "rolePing": "",
            "events": {
                "error": true, "skip": true, "recovery": true, "critical": true, "resume": true,
                "streamDrop": true, "streamRestart": true, "outputFrozen": true,
                "twitchMismatch": true, "twitchRestart": true
            },
            "templates": {}
        },
        "supervisor": {
            "ingestUrl": "rtmp://live.twitch.tv/app", "encoder": "h264_nvenc", "cacheBudgetMb": 300,
            "readAheadSeconds": 20, "playerClient": "mweb,web_embedded", "overlay": true,
            "freezeSeconds": 20, "silenceSeconds": 30, "streamKeySet": true
        },
        "autoUpdateCheck": true,
        "updateCheckIntervalMs": 21_600_000,
        "autoUpdateYtdlp": true,
        "autoOpenAdmin": true,
        "autoStartStream": true,
        "twitchClientId": "",
        "twitchClientSecret": "",
        "twitchChannel": "",
        "twitchLivenessEnabled": false,
        "twitchPollIntervalMs": 60_000,
        "ytdlCookiesFromBrowser": ""
    })
}

/// A setup check report: NVENC works, 4096 MB recommended, ffmpeg found and Deno missing (listed found first).
pub fn setup_report_json() -> Value {
    json!({
        "at": "2026-11-08T12:34:00.000Z",
        "encoder": { "chosen": "h264_nvenc", "tried": ["h264_nvenc"], "software": false, "works": true },
        "cache": { "freeMb": 120_000, "recommendedMb": 4096, "warning": null },
        "dependencies": [
            { "name": "ffmpeg", "ok": true, "path": r"C:\StreamLoop3\streamloop\ffmpeg\ffmpeg.exe" },
            { "name": "Deno", "ok": false, "path": r"C:\StreamLoop3\streamloop\yt-dlp\deno.exe" }
        ]
    })
}

/// `POST /api/config`'s answer after a reload that worked.
pub fn saved_json(restart_required: bool) -> Value {
    json!({
        "ok": true, "config": config_json(), "restartRequired": restart_required,
        "reloadError": null, "reloadErrorCode": null
    })
}

/// `GET /api/events`, oldest first, one second apart: "Event 1" to "Event n".
pub fn events_json(n: usize) -> Value {
    let events: Vec<Value> = (1..=n)
        .map(|i| json!({ "timestamp": format!("2026-11-08T12:{:02}:{:02}.000Z", i / 60, i % 60), "message": format!("Event {i}") }))
        .collect();
    Value::Array(events)
}

/// `GET /api/update/status`; any `status` but `idle` has 3.0.6 waiting, and `error` carries its message.
pub fn update_json(status: &str) -> Value {
    let available = status != "idle";
    json!({
        "currentVersion": "3.0.5",
        "latestVersion": if available { "3.0.6" } else { "3.0.5" },
        "updateAvailable": available,
        "status": status,
        "error": if status == "error" { Some("Checksum mismatch") } else { None },
        "isDevMode": false,
        "nodeNotice": null
    })
}

/// `GET /api/update/status` with 3.0.6 waiting, also in `idle`, the state after a check found it.
pub fn offered_json(status: &str) -> Value {
    let mut update = update_json(status);
    update["updateAvailable"] = json!(true);
    update["latestVersion"] = json!("3.0.6");
    update
}

/// Node's 409 for a change while an update runs (api.ts `UPDATE_RUNNING`).
pub const UPDATE_RUNNING: &str = "An update is running. Try again when it is done.";

/// `update-state.json` for the full update from 3.0.5 to 3.0.6 of Node and ffmpeg, at `step` with
/// `result`; the step started `ago_s` seconds before the test clock's now.
pub fn progress_json(cx: &mut TestAppContext, step: &str, result: &str, ago_s: f64) -> Value {
    let now = cx.update(|cx| crate::app::now_ms(cx));
    let at = chrono::DateTime::from_timestamp_millis(now - (ago_s * 1000.0) as i64)
        .expect("a time in range")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    json!({
        "from": "3.0.5", "to": "3.0.6", "parts": ["node", "ffmpeg"], "folders": ["node", "ffmpeg"],
        "step": step, "result": result, "reason": null, "startedAt": at, "stepStartedAt": at,
        "baseline": null
    })
}

/// `GET /api/update/status` with the full update of Node and ffmpeg to 3.0.6 on offer in `status`,
/// `progress` as Node reads it from `update-state.json` and an empty swap log.
pub fn full_offer_json(status: &str, progress: Value) -> Value {
    let mut update = offered_json(status);
    update["kind"] = json!("full");
    update["parts"] = json!(["node", "ffmpeg"]);
    update["sameVersion"] = json!(false);
    update["downloadPercent"] = Value::Null;
    update["progress"] = progress;
    update["swapLog"] = json!([]);
    update
}
